use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, OwnedMutexGuard};
use url::Url;
use uuid::Uuid;

use crate::{
    orchestration::AttachmentReference,
    transfer::staging::{UploadBinding, UploadError, UploadOwner, UploadRegistry, UploadTarget},
};

pub(crate) const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
const MAX_ATTACHMENTS: usize = 8;
const MAX_ENCODED_ATTACHMENT_BYTES: usize = 4 * MAX_ATTACHMENT_BYTES.div_ceil(3);
const MAX_ATTACHMENT_NAME_LENGTH: usize = 255;
const MAX_ATTACHED_FILES_TEXT_BYTES: usize = 16 * 1024;

/// Attachments a turn may send again by id alone, without their bytes: files an accepted command
/// already attached in the same thread, keyed by id, with the content digest recorded then
/// (references backfilled from legacy events have none).
pub(crate) type ReusableAttachments = HashMap<String, Option<String>>;

#[derive(Clone, Debug)]
pub(crate) struct AttachmentMaterializer {
    state_dir: PathBuf,
    attachments_dir: PathBuf,
    root_initialized: Arc<AtomicBool>,
    root_transaction: Arc<Mutex<()>>,
    #[cfg(test)]
    force_copy: bool,
    #[cfg(test)]
    after_stage_write: Option<Arc<AttachmentPrepareTestPause>>,
    #[cfg(test)]
    after_final_publication: Option<Arc<AttachmentPrepareTestPause>>,
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct AttachmentPrepareTestPause {
    reached: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}

#[cfg(test)]
impl AttachmentPrepareTestPause {
    pub(crate) async fn wait_until_reached(&self) {
        self.reached.notified().await;
    }

    pub(crate) fn release(&self) {
        self.resume.notify_one();
    }
}

#[derive(Debug)]
pub(crate) struct PreparedAttachmentBatch {
    attachments: Vec<Value>,
    references: Vec<AttachmentReference>,
    owned_finals: Vec<PathBuf>,
    owned_stages: Vec<PathBuf>,
    bound_uploads: Vec<Arc<UploadBinding>>,
    _root_transaction: Option<Arc<OwnedMutexGuard<()>>>,
}

impl PreparedAttachmentBatch {
    fn new(capacity: usize, root_transaction: Option<OwnedMutexGuard<()>>) -> Self {
        Self {
            attachments: Vec::with_capacity(capacity),
            references: Vec::with_capacity(capacity),
            owned_finals: Vec::new(),
            owned_stages: Vec::new(),
            bound_uploads: Vec::new(),
            _root_transaction: root_transaction.map(Arc::new),
        }
    }

    pub(crate) fn attachments(&self) -> &[Value] {
        &self.attachments
    }

    pub(crate) fn references(&self) -> &[AttachmentReference] {
        &self.references
    }

    pub(crate) fn commit(mut self) {
        self.owned_finals.clear();
        for binding in self.bound_uploads.drain(..) {
            Arc::try_unwrap(binding)
                .expect("publication released its binding lease")
                .commit();
        }
    }

    fn remove_stage(
        &mut self,
        staged: &Path,
        id: &str,
    ) -> Result<(), AttachmentMaterializationError> {
        match std::fs::remove_file(staged) {
            Ok(()) => {
                self.owned_stages.retain(|path| path != staged);
                Ok(())
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                self.owned_stages.retain(|path| path != staged);
                Ok(())
            }
            Err(source) => Err(AttachmentMaterializationError::Write {
                id: id.to_owned(),
                source,
            }),
        }
    }
}

impl Drop for PreparedAttachmentBatch {
    fn drop(&mut self) {
        for path in self.owned_stages.iter().rev() {
            let _ = std::fs::remove_file(path);
        }
        for path in self.owned_finals.iter().rev() {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MaterializedAttachment {
    pub attachment_type: String,
    pub name: String,
    pub mime_type: String,
    pub base64_data: String,
    pub file_url: String,
    pub path: PathBuf,
}

#[derive(Debug, Error)]
pub(crate) enum AttachmentMaterializationError {
    #[error(transparent)]
    Upload(#[from] UploadError),
    #[error("invalid attachment metadata: {0}")]
    InvalidMetadata(String),
    #[error("invalid attachment id {0}")]
    InvalidId(String),
    #[error("attachment {0} was not attached earlier in this thread; attach the file again")]
    NotReusable(String),
    #[error("failed to access attachment directory {path}: {source}")]
    AttachmentDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to read attachment {id}: {source}")]
    Read { id: String, source: std::io::Error },
    #[error("failed to write attachment {id}: {source}")]
    Write { id: String, source: std::io::Error },
    #[error("attachment {0} resolves outside the attachment directory")]
    EscapesDirectory(String),
    #[error("attachment {0} cannot be represented as a file URL")]
    InvalidFileUrl(String),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AttachmentInput {
    #[serde(rename = "type")]
    attachment_type: String,
    id: String,
    name: String,
    mime_type: String,
    size_bytes: u64,
    #[serde(default)]
    data_url: Option<String>,
    #[serde(default)]
    upload_id: Option<String>,
}

impl AttachmentMaterializer {
    pub(crate) fn new(attachments_dir: PathBuf) -> Self {
        let state_dir = attachments_dir
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        Self {
            state_dir,
            attachments_dir,
            root_initialized: Arc::new(AtomicBool::new(false)),
            // ponytail: this root-wide in-process lock assumes one server process per state root;
            // add an OS file lock if shared multi-process state roots become supported.
            root_transaction: Arc::new(Mutex::new(())),
            #[cfg(test)]
            force_copy: false,
            #[cfg(test)]
            after_stage_write: None,
            #[cfg(test)]
            after_final_publication: None,
        }
    }

    #[cfg(test)]
    fn with_forced_copy_for_test(mut self) -> Self {
        self.force_copy = true;
        self
    }

    #[cfg(test)]
    fn with_pause_after_stage_write(mut self, pause: Arc<AttachmentPrepareTestPause>) -> Self {
        self.after_stage_write = Some(pause);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_pause_after_final_publication(
        mut self,
        pause: Arc<AttachmentPrepareTestPause>,
    ) -> Self {
        self.after_final_publication = Some(pause);
        self
    }

    /// Publishes each upload (`dataUrl`) under its id. An attachment sent by id alone must be one
    /// of `reusable`; any other id is refused, even when a file with that id exists.
    pub(crate) async fn prepare(
        &self,
        attachments: Vec<Value>,
        reusable: &ReusableAttachments,
        owner: &UploadOwner,
        uploads: &UploadRegistry,
    ) -> Result<PreparedAttachmentBatch, AttachmentMaterializationError> {
        if attachments.is_empty() {
            return Ok(PreparedAttachmentBatch::new(0, None));
        }
        if attachments.len() > MAX_ATTACHMENTS {
            return Err(too_many_attachments());
        }
        let transaction = self.root_transaction.clone().lock_owned().await;
        let mut prepared = PreparedAttachmentBatch::new(attachments.len(), Some(transaction));
        let root = self.canonical_root(true).await?;
        if !self.root_initialized.load(Ordering::Acquire) {
            self.scavenge_stages(&root).await?;
            self.root_initialized.store(true, Ordering::Release);
        }
        let mut upload_ids = HashSet::new();
        for value in attachments {
            let upload_id_present = value.get("uploadId").is_some();
            let data_url_present = value.get("dataUrl").is_some();
            let attachment: AttachmentInput = serde_json::from_value(value).map_err(|error| {
                AttachmentMaterializationError::InvalidMetadata(error.to_string())
            })?;
            validate_attachment(&attachment)?;
            let content_digest = if upload_id_present {
                if data_url_present {
                    return Err(AttachmentMaterializationError::InvalidMetadata(
                        "Attachment cannot contain both dataUrl and uploadId".into(),
                    ));
                }
                let id = attachment
                    .upload_id
                    .as_deref()
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| {
                        AttachmentMaterializationError::InvalidMetadata(
                            "uploadId must be a nonempty string".into(),
                        )
                    })?;
                if !upload_ids.insert(id.to_owned()) {
                    return Err(AttachmentMaterializationError::InvalidMetadata(
                        "A staged upload cannot appear twice in a turn".into(),
                    ));
                }
                let target = UploadTarget::ChatAttachment {
                    attachment_type: attachment.attachment_type.clone(),
                    name: attachment.name.clone(),
                    mime_type: attachment.mime_type.clone(),
                };
                let binding = Arc::new(
                    uploads
                        .bind(owner, id, &target, attachment.size_bytes)
                        .await?,
                );
                let content_digest = binding.digest().to_owned();
                self.publish_staged(&root, &attachment.id, &binding, &mut prepared)
                    .await?;
                prepared.bound_uploads.push(binding);
                content_digest
            } else {
                match (data_url_present, attachment.data_url.as_deref()) {
                    (true, Some(data_url)) => {
                        let bytes = decode_data_url(data_url, &attachment.mime_type)?;
                        if bytes.len()
                            != usize::try_from(attachment.size_bytes).unwrap_or(usize::MAX)
                        {
                            return Err(AttachmentMaterializationError::InvalidMetadata(
                                "claimed size does not match decoded data".to_owned(),
                            ));
                        }
                        self.publish(&root, &attachment.id, &bytes, &mut prepared)
                            .await?;
                        crate::crypto::sha256_hex(&bytes)
                    }
                    (false, None) => {
                        let recorded_digest = reusable.get(&attachment.id).ok_or_else(|| {
                            AttachmentMaterializationError::NotReusable(attachment.id.clone())
                        })?;
                        let existing = self.read_canonical(&root, &attachment.id).await?;
                        if existing.len()
                            != usize::try_from(attachment.size_bytes).unwrap_or(usize::MAX)
                        {
                            return Err(AttachmentMaterializationError::InvalidMetadata(
                                "claimed size does not match prepared file".to_owned(),
                            ));
                        }
                        let digest = crate::crypto::sha256_hex(&existing);
                        if recorded_digest
                            .as_ref()
                            .is_some_and(|recorded| *recorded != digest)
                        {
                            return Err(AttachmentMaterializationError::InvalidMetadata(
                                "prepared file does not match the attachment sent earlier"
                                    .to_owned(),
                            ));
                        }
                        digest
                    }
                    _ => {
                        return Err(AttachmentMaterializationError::InvalidMetadata(
                            "dataUrl must be a base64 string when present".to_owned(),
                        ));
                    }
                }
            };
            let canonical_path = self.canonical_path(&root, &attachment.id).await?;
            debug_assert!(canonical_path.starts_with(&root));
            prepared.attachments.push(serde_json::json!({
                "type": attachment.attachment_type,
                "id": attachment.id,
                "name": attachment.name,
                "mimeType": attachment.mime_type,
                "sizeBytes": attachment.size_bytes,
            }));
            prepared.references.push(AttachmentReference {
                attachment_id: attachment.id,
                content_digest: Some(content_digest),
                size_bytes: i64::try_from(attachment.size_bytes)
                    .expect("validated attachment size"),
            });
        }
        Ok(prepared)
    }

    pub(crate) async fn reconcile_startup(
        &self,
        referenced: &HashSet<String>,
    ) -> Result<(), AttachmentMaterializationError> {
        let _transaction = self.root_transaction.clone().lock_owned().await;
        let root = self.canonical_root(true).await?;
        let mut entries = tokio::fs::read_dir(&root).await.map_err(|source| {
            AttachmentMaterializationError::AttachmentDirectory {
                path: root.clone(),
                source,
            }
        })?;
        while let Some(entry) = entries.next_entry().await.map_err(|source| {
            AttachmentMaterializationError::AttachmentDirectory {
                path: root.clone(),
                source,
            }
        })? {
            let metadata = tokio::fs::symlink_metadata(entry.path())
                .await
                .map_err(
                    |source| AttachmentMaterializationError::AttachmentDirectory {
                        path: root.clone(),
                        source,
                    },
                )?;
            if metadata.file_type().is_symlink()
                || is_reparse_point(&metadata)
                || !metadata.is_file()
            {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.starts_with('.') && name.ends_with(".upload") {
                remove_attachment_leaf(&entry.path(), &root).await?;
                continue;
            }
            if validate_attachment_id(name).is_err() {
                continue;
            }
            match self.canonical_path(&root, name).await {
                Ok(_) if referenced.contains(name) => {}
                Ok(path) => remove_attachment_leaf(&path, &root).await?,
                Err(AttachmentMaterializationError::EscapesDirectory(_)) => {}
                Err(error) => return Err(error),
            }
        }
        self.root_initialized.store(true, Ordering::Release);
        Ok(())
    }

    pub(crate) async fn materialize(
        &self,
        attachments: Vec<Value>,
    ) -> Result<Vec<MaterializedAttachment>, AttachmentMaterializationError> {
        if attachments.is_empty() {
            return Ok(Vec::new());
        }
        if attachments.len() > MAX_ATTACHMENTS {
            return Err(too_many_attachments());
        }
        let root = self.canonical_root(false).await?;
        let mut materialized = Vec::with_capacity(attachments.len());
        for attachment in attachments {
            if attachment.get("dataUrl").is_some() || attachment.get("uploadId").is_some() {
                return Err(AttachmentMaterializationError::InvalidMetadata(
                    "prepared attachments cannot contain dataUrl".to_owned(),
                ));
            }
            let attachment: AttachmentInput =
                serde_json::from_value(attachment).map_err(|error| {
                    AttachmentMaterializationError::InvalidMetadata(error.to_string())
                })?;
            validate_attachment(&attachment)?;
            let path = self.canonical_path(&root, &attachment.id).await?;
            let bytes = self.read_canonical(&root, &attachment.id).await?;
            if bytes.len() != usize::try_from(attachment.size_bytes).unwrap_or(usize::MAX) {
                return Err(AttachmentMaterializationError::InvalidMetadata(
                    "claimed size does not match prepared file".to_owned(),
                ));
            }
            let file_url = Url::from_file_path(&path)
                .map_err(|()| AttachmentMaterializationError::InvalidFileUrl(attachment.id))?
                .to_string();
            materialized.push(MaterializedAttachment {
                attachment_type: attachment.attachment_type,
                name: attachment.name,
                mime_type: attachment.mime_type,
                base64_data: STANDARD.encode(bytes),
                file_url,
                path,
            });
        }
        Ok(materialized)
    }

    pub(crate) async fn resolve_existing_file(
        &self,
        id: &str,
    ) -> Result<PathBuf, AttachmentMaterializationError> {
        validate_attachment_id(id)?;
        let root = self.canonical_root(false).await?;
        self.canonical_path(&root, id).await
    }

    async fn canonical_root(
        &self,
        create: bool,
    ) -> Result<PathBuf, AttachmentMaterializationError> {
        if create {
            tokio::fs::create_dir_all(&self.state_dir)
                .await
                .map_err(
                    |source| AttachmentMaterializationError::AttachmentDirectory {
                        path: self.state_dir.clone(),
                        source,
                    },
                )?;
        }
        let state_dir = tokio::fs::canonicalize(&self.state_dir)
            .await
            .map_err(
                |source| AttachmentMaterializationError::AttachmentDirectory {
                    path: self.state_dir.clone(),
                    source,
                },
            )?;
        match tokio::fs::symlink_metadata(&self.attachments_dir).await {
            Ok(metadata) if metadata.file_type().is_symlink() || is_reparse_point(&metadata) => {
                return Err(AttachmentMaterializationError::EscapesDirectory(
                    "attachments".to_owned(),
                ));
            }
            Ok(_) => {}
            Err(source) if create && source.kind() == std::io::ErrorKind::NotFound => {
                match tokio::fs::create_dir(&self.attachments_dir).await {
                    Ok(()) => {}
                    Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(source) => {
                        return Err(AttachmentMaterializationError::AttachmentDirectory {
                            path: self.attachments_dir.clone(),
                            source,
                        });
                    }
                }
            }
            Err(source) => {
                return Err(AttachmentMaterializationError::AttachmentDirectory {
                    path: self.attachments_dir.clone(),
                    source,
                });
            }
        }
        let metadata = tokio::fs::symlink_metadata(&self.attachments_dir)
            .await
            .map_err(
                |source| AttachmentMaterializationError::AttachmentDirectory {
                    path: self.attachments_dir.clone(),
                    source,
                },
            )?;
        if metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
            return Err(AttachmentMaterializationError::EscapesDirectory(
                "attachments".to_owned(),
            ));
        }
        if !metadata.is_dir() {
            return Err(AttachmentMaterializationError::AttachmentDirectory {
                path: self.attachments_dir.clone(),
                source: std::io::Error::other("attachment root is not a directory"),
            });
        }
        tokio::fs::canonicalize(&self.attachments_dir)
            .await
            .and_then(|root| {
                if root.starts_with(&state_dir) {
                    Ok(root)
                } else {
                    Err(std::io::Error::other(
                        "attachment root escapes state directory",
                    ))
                }
            })
            .map_err(
                |source| AttachmentMaterializationError::AttachmentDirectory {
                    path: self.attachments_dir.clone(),
                    source,
                },
            )
    }

    async fn canonical_path(
        &self,
        root: &Path,
        id: &str,
    ) -> Result<PathBuf, AttachmentMaterializationError> {
        let leaf = root.join(id);
        let metadata = tokio::fs::symlink_metadata(&leaf).await.map_err(|source| {
            AttachmentMaterializationError::Read {
                id: id.to_owned(),
                source,
            }
        })?;
        if metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
            return Err(AttachmentMaterializationError::EscapesDirectory(
                id.to_owned(),
            ));
        }
        if !metadata.is_file() {
            return Err(AttachmentMaterializationError::Read {
                id: id.to_owned(),
                source: std::io::Error::other("attachment is not a regular file"),
            });
        }
        let path = tokio::fs::canonicalize(leaf).await.map_err(|source| {
            AttachmentMaterializationError::Read {
                id: id.to_owned(),
                source,
            }
        })?;
        if path.starts_with(root) {
            Ok(path)
        } else {
            Err(AttachmentMaterializationError::EscapesDirectory(
                id.to_owned(),
            ))
        }
    }

    async fn read_canonical(
        &self,
        root: &Path,
        id: &str,
    ) -> Result<Vec<u8>, AttachmentMaterializationError> {
        let path = self.canonical_path(root, id).await?;
        let file = tokio::fs::File::open(path).await.map_err(|source| {
            AttachmentMaterializationError::Read {
                id: id.to_owned(),
                source,
            }
        })?;
        let mut bytes = Vec::with_capacity(MAX_ATTACHMENT_BYTES.min(4096));
        file.take((MAX_ATTACHMENT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|source| AttachmentMaterializationError::Read {
                id: id.to_owned(),
                source,
            })?;
        if bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(AttachmentMaterializationError::InvalidMetadata(
                "prepared attachment exceeds 10 MiB".to_owned(),
            ));
        }
        Ok(bytes)
    }

    async fn publish(
        &self,
        root: &Path,
        id: &str,
        bytes: &[u8],
        prepared: &mut PreparedAttachmentBatch,
    ) -> Result<(), AttachmentMaterializationError> {
        let staged = root.join(format!(".{id}.{}.upload", Uuid::new_v4()));
        let file =
            create_stage_file(&staged).map_err(|source| AttachmentMaterializationError::Write {
                id: id.to_owned(),
                source,
            })?;
        prepared.owned_stages.push(staged.clone());
        let mut file = tokio::fs::File::from_std(file);
        let write = async {
            file.write_all(bytes).await?;
            #[cfg(test)]
            if let Some(pause) = &self.after_stage_write {
                pause.reached.notify_one();
                pause.resume.notified().await;
            }
            file.flush().await
        }
        .await;
        if let Err(source) = write {
            return Err(AttachmentMaterializationError::Write {
                id: id.to_owned(),
                source,
            });
        }
        drop(file);
        let publication = publish_stage_file(
            staged.clone(),
            root.join(id),
            false,
            prepared._root_transaction.clone(),
            None,
        )
        .await
        .map_err(|source| AttachmentMaterializationError::Write {
            id: id.into(),
            source,
        })?;
        if let Some(path) = publication.into_owned_path() {
            prepared.owned_finals.push(path);
            #[cfg(test)]
            if let Some(pause) = &self.after_final_publication {
                pause.reached.notify_one();
                pause.resume.notified().await;
            }
            prepared.remove_stage(&staged, id)?;
            Ok(())
        } else {
            let existing = self.read_canonical(root, id).await;
            prepared.remove_stage(&staged, id)?;
            match existing {
                Ok(existing) if existing == bytes => Ok(()),
                Ok(_) => Err(AttachmentMaterializationError::InvalidMetadata(
                    "attachment id already exists with different content".into(),
                )),
                Err(error) => Err(error),
            }
        }
    }

    async fn publish_staged(
        &self,
        root: &Path,
        id: &str,
        binding: &Arc<UploadBinding>,
        prepared: &mut PreparedAttachmentBatch,
    ) -> Result<(), AttachmentMaterializationError> {
        let source = tokio::fs::File::open(binding.path())
            .await
            .map_err(|source| AttachmentMaterializationError::Read {
                id: id.into(),
                source,
            })?;
        let mut bytes = Vec::new();
        source
            .take((MAX_ATTACHMENT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|source| AttachmentMaterializationError::Read {
                id: id.into(),
                source,
            })?;
        if bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(AttachmentMaterializationError::InvalidMetadata(
                "Staged file exceeds attachment limit".into(),
            ));
        }
        if crate::crypto::sha256_hex(&bytes) != binding.digest() {
            return Err(AttachmentMaterializationError::InvalidMetadata(
                "Staged file digest changed".into(),
            ));
        }
        #[cfg(test)]
        let force_copy = self.force_copy;
        #[cfg(not(test))]
        let force_copy = false;
        let publication = publish_stage_file(
            binding.path().to_path_buf(),
            root.join(id),
            force_copy,
            prepared._root_transaction.clone(),
            Some(binding.clone()),
        )
        .await
        .map_err(|source| AttachmentMaterializationError::Write {
            id: id.into(),
            source,
        })?;
        if let Some(path) = publication.into_owned_path() {
            prepared.owned_finals.push(path);
            Ok(())
        } else if self.read_canonical(root, id).await? == bytes {
            Ok(())
        } else {
            Err(AttachmentMaterializationError::InvalidMetadata(
                "attachment id already exists with different content".into(),
            ))
        }
    }

    async fn scavenge_stages(&self, root: &Path) -> Result<(), AttachmentMaterializationError> {
        let mut entries = tokio::fs::read_dir(root).await.map_err(|source| {
            AttachmentMaterializationError::AttachmentDirectory {
                path: root.to_path_buf(),
                source,
            }
        })?;
        while let Some(entry) = entries.next_entry().await.map_err(|source| {
            AttachmentMaterializationError::AttachmentDirectory {
                path: root.to_path_buf(),
                source,
            }
        })? {
            let metadata = tokio::fs::symlink_metadata(entry.path())
                .await
                .map_err(
                    |source| AttachmentMaterializationError::AttachmentDirectory {
                        path: root.to_path_buf(),
                        source,
                    },
                )?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if metadata.is_file()
                && !metadata.file_type().is_symlink()
                && !is_reparse_point(&metadata)
                && name.starts_with('.')
                && name.ends_with(".upload")
            {
                remove_attachment_leaf(&entry.path(), root).await?;
            }
        }
        Ok(())
    }
}

async fn remove_attachment_leaf(
    path: &Path,
    root: &Path,
) -> Result<(), AttachmentMaterializationError> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(AttachmentMaterializationError::AttachmentDirectory {
            path: root.to_path_buf(),
            source,
        }),
    }
}

pub(crate) fn prompt_parts(text: Option<&str>, attachments: Vec<Value>) -> Vec<Value> {
    let mut parts = Vec::with_capacity(attachments.len() + usize::from(text.is_some()));
    if let Some(text) = text.filter(|text| !text.is_empty()) {
        parts.push(serde_json::json!({ "type": "text", "text": text }));
    }
    parts.extend(attachments);
    parts
}

/// The distinct ids of the attachments sent by id alone, for the caller to look up before
/// [`prepare`]. The attachment cap and each id's shape are checked first, so a lookup sees at
/// most [`MAX_ATTACHMENTS`] well-formed ids.
///
/// [`prepare`]: AttachmentMaterializer::prepare
pub(crate) fn id_only_attachment_ids(
    attachments: &[Value],
) -> Result<Vec<String>, AttachmentMaterializationError> {
    if attachments.len() > MAX_ATTACHMENTS {
        return Err(too_many_attachments());
    }
    let mut ids = Vec::<String>::new();
    for attachment in attachments.iter().filter(|attachment| {
        attachment.get("dataUrl").is_none() && attachment.get("uploadId").is_none()
    }) {
        let Some(id) = attachment.get("id").and_then(Value::as_str) else {
            continue;
        };
        validate_attachment_id(id)?;
        if !ids.iter().any(|known| known == id) {
            ids.push(id.to_owned());
        }
    }
    Ok(ids)
}

fn too_many_attachments() -> AttachmentMaterializationError {
    AttachmentMaterializationError::InvalidMetadata(
        "at most eight attachments are allowed".to_owned(),
    )
}

fn validate_attachment_id(id: &str) -> Result<(), AttachmentMaterializationError> {
    let mut components = Path::new(id).components();
    let valid_component =
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none();
    let valid_characters = !id.is_empty()
        && id.len() <= 128
        && !is_windows_reserved_name(id)
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
    if valid_component && valid_characters {
        Ok(())
    } else {
        Err(AttachmentMaterializationError::InvalidId(id.to_owned()))
    }
}

fn validate_attachment(attachment: &AttachmentInput) -> Result<(), AttachmentMaterializationError> {
    validate_attachment_id(&attachment.id)?;
    let valid_name = !attachment.name.is_empty()
        && attachment.name.trim() == attachment.name
        && attachment.name.encode_utf16().count() <= MAX_ATTACHMENT_NAME_LENGTH
        && attachment.name != "."
        && attachment.name != ".."
        && !attachment
            .name
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\'));
    if !valid_name {
        return Err(AttachmentMaterializationError::InvalidMetadata(
            "invalid attachment name".to_owned(),
        ));
    }
    let valid_mime = !attachment.mime_type.is_empty()
        && attachment.mime_type.trim() == attachment.mime_type
        && attachment.mime_type.len() <= 100
        && attachment
            .mime_type
            .split_once('/')
            .is_some_and(|(kind, subtype)| {
                !kind.is_empty()
                    && !subtype.is_empty()
                    && kind.bytes().chain(subtype.bytes()).all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-')
                    })
            });
    let matching_type = match attachment.attachment_type.as_str() {
        "image" => is_image_mime(&attachment.mime_type),
        "file" => !is_image_mime(&attachment.mime_type),
        _ => false,
    };
    if !valid_mime || !matching_type {
        return Err(AttachmentMaterializationError::InvalidMetadata(
            "attachment type and MIME type do not match".to_owned(),
        ));
    }
    if attachment.size_bytes > MAX_ATTACHMENT_BYTES as u64 {
        return Err(AttachmentMaterializationError::InvalidMetadata(
            "attachment exceeds 10 MiB".to_owned(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_upload_metadata(
    attachment_type: &str,
    name: &str,
    mime_type: &str,
    size_bytes: u64,
) -> Result<(), AttachmentMaterializationError> {
    validate_attachment(&AttachmentInput {
        attachment_type: attachment_type.into(),
        id: "staging".into(),
        name: name.into(),
        mime_type: mime_type.into(),
        size_bytes,
        data_url: None,
        upload_id: None,
    })
}

fn decode_data_url(
    data_url: &str,
    expected_mime: &str,
) -> Result<Vec<u8>, AttachmentMaterializationError> {
    let (mime_type, encoded) = data_url
        .strip_prefix("data:")
        .and_then(|value| value.split_once(";base64,"))
        .filter(|(mime_type, _)| *mime_type == expected_mime && !mime_type.contains(';'))
        .ok_or_else(|| {
            AttachmentMaterializationError::InvalidMetadata(
                "attachment dataUrl must be base64 with the declared MIME type".to_owned(),
            )
        })?;
    let _ = mime_type;
    if encoded.len() > MAX_ENCODED_ATTACHMENT_BYTES {
        return Err(AttachmentMaterializationError::InvalidMetadata(
            "attachment dataUrl exceeds the base64 size limit".to_owned(),
        ));
    }
    let decoded = STANDARD.decode(encoded).map_err(|_| {
        AttachmentMaterializationError::InvalidMetadata(
            "attachment dataUrl contains invalid base64".to_owned(),
        )
    })?;
    if decoded.len() > MAX_ATTACHMENT_BYTES {
        return Err(AttachmentMaterializationError::InvalidMetadata(
            "attachment exceeds 10 MiB".to_owned(),
        ));
    }
    Ok(decoded)
}

pub(crate) fn split_native_images_and_file_references(
    attachments: Vec<MaterializedAttachment>,
) -> (Vec<MaterializedAttachment>, Vec<MaterializedAttachment>) {
    attachments
        .into_iter()
        .partition(|attachment| attachment.attachment_type == "image")
}

pub(crate) fn append_file_references(
    text: String,
    files: &[MaterializedAttachment],
) -> Result<String, AttachmentMaterializationError> {
    if files.is_empty() {
        return Ok(text);
    }
    let mut section = String::from("\n<attached_files>\n");
    for file in files {
        let path = file.path.to_str().ok_or_else(|| {
            AttachmentMaterializationError::InvalidMetadata(
                "attachment path is not Unicode".to_owned(),
            )
        })?;
        if path.chars().any(char::is_control) {
            return Err(AttachmentMaterializationError::InvalidMetadata(
                "attachment path contains control characters".to_owned(),
            ));
        }
        section.push_str("- ");
        section.push_str(&escape_xml(&file.name));
        section.push_str(": ");
        section.push_str(&escape_xml(path));
        section.push('\n');
        if section.len() > MAX_ATTACHED_FILES_TEXT_BYTES {
            return Err(AttachmentMaterializationError::InvalidMetadata(
                "attached file references exceed 16 KiB".to_owned(),
            ));
        }
    }
    section.push_str("</attached_files>");
    if section.len() > MAX_ATTACHED_FILES_TEXT_BYTES {
        return Err(AttachmentMaterializationError::InvalidMetadata(
            "attached file references exceed 16 KiB".to_owned(),
        ));
    }
    Ok(text + &section)
}

fn is_image_mime(mime: &str) -> bool {
    mime.get(..6)
        .is_some_and(|kind| kind.eq_ignore_ascii_case("image/"))
}

#[cfg(test)]
static PUBLICATION_PAUSES: std::sync::OnceLock<
    std::sync::Mutex<HashMap<PathBuf, Arc<AttachmentPrepareTestPause>>>,
> = std::sync::OnceLock::new();

/// Owns a published final through blocking I/O and result delivery. A canceled
/// awaiting caller drops the result, so publication cannot outlive rollback.
struct AttachmentPublication {
    owned_path: Option<PathBuf>,
    _root_transaction: Option<Arc<OwnedMutexGuard<()>>>,
    _binding: Option<Arc<UploadBinding>>,
}
impl AttachmentPublication {
    fn into_owned_path(mut self) -> Option<PathBuf> {
        self.owned_path.take()
    }
}
impl Drop for AttachmentPublication {
    fn drop(&mut self) {
        if let Some(path) = &self.owned_path {
            let _ = std::fs::remove_file(path);
        }
    }
}
async fn publish_stage_file(
    staged: PathBuf,
    final_path: PathBuf,
    force_copy: bool,
    root_transaction: Option<Arc<OwnedMutexGuard<()>>>,
    binding: Option<Arc<UploadBinding>>,
) -> std::io::Result<AttachmentPublication> {
    #[cfg(test)]
    let pause = PUBLICATION_PAUSES
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .get(&final_path)
        .cloned();
    tokio::task::spawn_blocking(move || {
        let linked = if force_copy {
            Err(std::io::Error::other("forced copy"))
        } else {
            std::fs::hard_link(&staged, &final_path)
        };
        let publication = match linked {
            Ok(()) => Ok::<AttachmentPublication, std::io::Error>(AttachmentPublication {
                owned_path: Some(final_path),
                _root_transaction: root_transaction,
                _binding: binding,
            }),
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                Ok(AttachmentPublication {
                    owned_path: None,
                    _root_transaction: root_transaction,
                    _binding: binding,
                })
            }
            Err(_) => {
                let file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&final_path);
                let mut file = match file {
                    Ok(file) => file,
                    Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                        return Ok(AttachmentPublication {
                            owned_path: None,
                            _root_transaction: root_transaction,
                            _binding: binding,
                        });
                    }
                    Err(source) => return Err(source),
                };
                let publication = AttachmentPublication {
                    owned_path: Some(final_path),
                    _root_transaction: root_transaction,
                    _binding: binding,
                };
                let source = std::fs::File::open(staged)?;
                let copied = std::io::copy(
                    &mut std::io::Read::take(source, (MAX_ATTACHMENT_BYTES + 1) as u64),
                    &mut file,
                )?;
                if copied > MAX_ATTACHMENT_BYTES as u64 {
                    return Err(std::io::Error::other(
                        "staged attachment exceeds size limit",
                    ));
                }
                std::io::Write::flush(&mut file)?;
                Ok(publication)
            }
        }?;
        #[cfg(test)]
        if let Some(pause) = pause {
            pause.reached.notify_one();
            tokio::runtime::Handle::current().block_on(pause.resume.notified());
        }
        Ok(publication)
    })
    .await
    .map_err(std::io::Error::other)?
}

fn create_stage_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0x0000_0001 | 0x0000_0002 | 0x0000_0004);
    }
    options.open(path)
}

fn is_windows_reserved_name(id: &str) -> bool {
    matches!(
        id.to_ascii_uppercase().as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

#[cfg(windows)]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}
#[cfg(not(windows))]
fn is_reparse_point(_: &std::fs::Metadata) -> bool {
    false
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    fn inline_upload_owner() -> super::UploadOwner {
        super::UploadOwner::Unauthenticated
    }
    fn inline_upload_registry() -> super::UploadRegistry {
        super::UploadRegistry::new(
            std::path::PathBuf::from("unused-upload-stages"),
            crate::transfer::staging::UploadLimits::default(),
            std::sync::Arc::new(tokio::time::Instant::now),
        )
    }
    use super::{UploadOwner, UploadRegistry, UploadTarget};
    use std::time::Duration;

    use base64::Engine as _;
    use serde_json::{Value, json};
    use std::{collections::HashSet, path::PathBuf, process::Command, sync::Arc};
    use tempfile::TempDir;

    use super::{
        AttachmentMaterializationError, AttachmentMaterializer, ReusableAttachments, STANDARD,
    };

    /// Reusable ids without a recorded digest, as for references backfilled from legacy events.
    fn reusable(ids: &[&str]) -> ReusableAttachments {
        ids.iter().map(|id| ((*id).to_owned(), None)).collect()
    }

    const ATTACHMENT_ABORT_CHILD_DIR: &str = "CODDY_STUDIO_ATTACHMENT_ABORT_CHILD_DIR";
    const ATTACHMENT_ABORT_CHILD_READY: &str = "CODDY_STUDIO_ATTACHMENT_ABORT_CHILD_READY";

    #[test]
    fn attachment_final_publication_abort_child() {
        let Some(attachments_dir) = std::env::var_os(ATTACHMENT_ABORT_CHILD_DIR) else {
            return;
        };
        let ready = PathBuf::from(
            std::env::var_os(ATTACHMENT_ABORT_CHILD_READY).expect("child ready path"),
        );
        let attachments_dir = PathBuf::from(attachments_dir);
        let pause = Arc::new(super::AttachmentPrepareTestPause::default());
        let materializer = AttachmentMaterializer::new(attachments_dir.clone())
            .with_pause_after_final_publication(pause.clone());
        let reached = pause.reached.notified();
        tokio::runtime::Runtime::new()
            .expect("child runtime")
            .block_on(async move {
                let _prepare = tokio::spawn(async move {
                    materializer
                        .prepare(
                            vec![json!({
                                "type":"file", "id":"aborted-final", "name":"notes.txt",
                                "mimeType":"text/plain", "sizeBytes":5,
                                "dataUrl":"data:text/plain;base64,bm90ZXM="
                            })],
                            &ReusableAttachments::new(),
                            &inline_upload_owner(),
                            &inline_upload_registry(),
                        )
                        .await
                });
                tokio::time::timeout(std::time::Duration::from_secs(5), reached)
                    .await
                    .expect("hard-link publication reaches the abort barrier");
                assert!(attachments_dir.join("aborted-final").exists());
                std::fs::write(ready, "published").expect("child ready marker");
                std::process::abort();
            });
    }

    #[tokio::test]
    async fn materializes_an_image_attachment_from_the_state_directory() {
        let state = TempDir::new().expect("state dir");
        let attachments_dir = state.path().join("attachments");
        tokio::fs::create_dir(&attachments_dir)
            .await
            .expect("attachments dir");
        tokio::fs::write(attachments_dir.join("image-1"), b"image bytes")
            .await
            .expect("attachment file");

        let attachments = AttachmentMaterializer::new(attachments_dir)
            .materialize(vec![json!({
                "type": "image",
                "id": "image-1",
                "name": "screen.png",
                "mimeType": "image/png",
                "sizeBytes": 11
            })])
            .await
            .expect("materialized image");

        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].attachment_type, "image");
        assert_eq!(attachments[0].name, "screen.png");
        assert_eq!(attachments[0].mime_type, "image/png");
        assert_eq!(attachments[0].base64_data, "aW1hZ2UgYnl0ZXM=");
        assert!(attachments[0].file_url.starts_with("file://"));
    }

    #[tokio::test]
    async fn startup_reconciliation_preserves_referenced_finals_and_removes_orphans_and_stages() {
        let state = TempDir::new().expect("state dir");
        let attachments_dir = state.path().join("attachments");
        tokio::fs::create_dir(&attachments_dir)
            .await
            .expect("attachments dir");
        tokio::fs::write(attachments_dir.join("keep-1"), b"keep")
            .await
            .expect("referenced final");
        tokio::fs::write(attachments_dir.join("orphan-1"), b"orphan")
            .await
            .expect("orphan final");
        tokio::fs::write(attachments_dir.join(".stale.upload"), b"partial")
            .await
            .expect("stale stage");

        let materializer = AttachmentMaterializer::new(attachments_dir.clone());
        let referenced = HashSet::from(["keep-1".to_owned()]);
        materializer.reconcile_startup(&referenced).await.unwrap();

        assert!(attachments_dir.join("keep-1").exists());
        assert!(!attachments_dir.join("orphan-1").exists());
        assert!(!attachments_dir.join(".stale.upload").exists());
    }

    #[tokio::test]
    async fn cancelling_after_stage_write_before_flush_removes_the_pending_stage_and_final() {
        let state = TempDir::new().expect("state dir");
        let attachments_dir = state.path().join("attachments");
        let pause = Arc::new(super::AttachmentPrepareTestPause::default());
        let materializer = AttachmentMaterializer::new(attachments_dir.clone())
            .with_pause_after_stage_write(pause.clone());
        let reached = pause.reached.notified();
        let task = tokio::spawn(async move {
            materializer
                .prepare(
                    vec![json!({
                        "type":"file", "id":"cancelled", "name":"notes.txt",
                        "mimeType":"text/plain", "sizeBytes":5,
                        "dataUrl":"data:text/plain;base64,bm90ZXM="
                    })],
                    &ReusableAttachments::new(),
                    &inline_upload_owner(),
                    &inline_upload_registry(),
                )
                .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(5), reached)
            .await
            .expect("write_all reaches the flush barrier");
        assert!(
            !task.is_finished(),
            "the preparation future remains pending before flush"
        );
        task.abort();
        pause.resume.notify_one();
        let _ = task.await;

        assert!(!attachments_dir.join("cancelled").exists());
        assert!(
            std::fs::read_dir(&attachments_dir)
                .expect("attachment entries")
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().ends_with(".upload")),
            "cancellation removes every owned stage"
        );
    }

    #[tokio::test]
    async fn startup_removes_a_final_left_by_a_process_aborted_after_publication() {
        let state = TempDir::new().expect("state dir");
        let config = crate::ServerConfig::new(state.path()).with_bind("127.0.0.1", 0);
        crate::test_support::hermetic_providers::write_hermetic_settings(
            &config.state_dir(),
            json!({}),
        );
        let attachments_dir = config.state_dir().join("attachments");
        let ready = state.path().join("published");
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "provider::attachments::tests::attachment_final_publication_abort_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(ATTACHMENT_ABORT_CHILD_DIR, &attachments_dir)
            .env(ATTACHMENT_ABORT_CHILD_READY, &ready)
            .output()
            .expect("run crash child");
        assert!(ready.exists(), "the child reached final publication");
        assert!(!output.status.success(), "the child must abort");
        assert!(attachments_dir.join("aborted-final").exists());

        let database = crate::persistence::Database::open_in_memory()
            .await
            .expect("database");
        database
            .call(|connection| {
                crate::persistence::run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let runtime = crate::production::runtime::ProductionRuntime::start(
            &config,
            database,
            crate::auth::AuthService::new(&config, vec![7_u8; 32]),
            vec![9_u8; 32],
            Arc::new(crate::diagnostics::NotApplicableUiProcessObserver),
        )
        .await
        .expect("restarted runtime");

        assert!(
            !attachments_dir.join("aborted-final").exists(),
            "startup reconciliation removes the unreferenced final"
        );
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn prepares_and_materializes_a_file_upload() {
        let state = TempDir::new().expect("state dir");
        let attachments_dir = state.path().join("attachments");
        let materializer = AttachmentMaterializer::new(attachments_dir.clone());

        let prepared_batch = materializer
            .prepare(
                vec![json!({
                    "type": "file",
                    "id": "notes-1",
                    "name": "notes.txt",
                    "mimeType": "text/plain",
                    "sizeBytes": 5,
                    "dataUrl": "data:text/plain;base64,bm90ZXM="
                })],
                &ReusableAttachments::new(),
                &inline_upload_owner(),
                &inline_upload_registry(),
            )
            .await
            .expect("file upload prepares");
        let prepared = prepared_batch.attachments().to_vec();
        assert_eq!(
            prepared,
            vec![json!({
                "type": "file",
                "id": "notes-1",
                "name": "notes.txt",
                "mimeType": "text/plain",
                "sizeBytes": 5
            })]
        );
        prepared_batch.commit();

        let attachments = materializer
            .materialize(prepared)
            .await
            .expect("file materializes");
        assert_eq!(attachments[0].attachment_type, "file");
        assert_eq!(attachments[0].base64_data, "bm90ZXM=");
        assert!(attachments_dir.join("notes-1").is_file());
    }

    #[tokio::test]
    async fn prepared_batch_exposes_sha256_references_for_every_attachment() {
        let state = TempDir::new().expect("state dir");
        let prepared = AttachmentMaterializer::new(state.path().join("attachments"))
            .prepare(
                vec![
                    json!({
                        "type":"file", "id":"a", "name":"a.txt", "mimeType":"text/plain",
                        "sizeBytes":1, "dataUrl":"data:text/plain;base64,YQ=="
                    }),
                    json!({
                        "type":"file", "id":"b", "name":"b.txt", "mimeType":"text/plain",
                        "sizeBytes":1, "dataUrl":"data:text/plain;base64,Yg=="
                    }),
                ],
                &ReusableAttachments::new(),
                &inline_upload_owner(),
                &inline_upload_registry(),
            )
            .await
            .expect("attachments prepare");

        assert_eq!(
            prepared
                .references()
                .iter()
                .map(|reference| (
                    reference.attachment_id.as_str(),
                    reference.content_digest.as_deref()
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    "a",
                    Some("ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb")
                ),
                (
                    "b",
                    Some("3e23e8160039594a33894f6564e1b1348bbd7a0088d42c4acb73eeaed59c009d")
                ),
            ]
        );
    }

    #[tokio::test]
    async fn dropping_a_prepared_batch_rolls_back_its_files() {
        let state = TempDir::new().expect("state dir");
        let attachment = state.path().join("attachments/drop-1");
        let prepared = AttachmentMaterializer::new(state.path().join("attachments"))
            .prepare(
                vec![json!({
                    "type":"file", "id":"drop-1", "name":"notes.txt", "mimeType":"text/plain",
                    "sizeBytes":5, "dataUrl":"data:text/plain;base64,bm90ZXM="
                })],
                &ReusableAttachments::new(),
                &inline_upload_owner(),
                &inline_upload_registry(),
            )
            .await
            .expect("upload prepares");
        assert!(attachment.is_file());

        drop(prepared);

        assert!(
            !attachment.exists(),
            "dropping ownership must roll back the final file"
        );
    }

    #[tokio::test]
    async fn rejects_attachment_ids_that_escape_the_state_directory() {
        let state = TempDir::new().expect("state dir");
        let attachments_dir = state.path().join("attachments");
        tokio::fs::create_dir(&attachments_dir)
            .await
            .expect("attachments dir");
        tokio::fs::write(state.path().join("outside"), b"secret")
            .await
            .expect("outside file");

        let error = AttachmentMaterializer::new(attachments_dir.clone())
            .materialize(vec![json!({
                "type": "image",
                "id": "../outside",
                "name": "outside.png",
                "mimeType": "image/png",
                "sizeBytes": 6
            })])
            .await
            .expect_err("traversal must fail");

        assert!(error.to_string().contains("invalid attachment id"));

        let missing_root_error = AttachmentMaterializer::new(state.path().join("missing"))
            .materialize(vec![json!({
                "type": "image",
                "id": "image-1",
                "name": "missing.png",
                "mimeType": "image/png",
                "sizeBytes": 11
            })])
            .await
            .expect_err("a missing attachment directory must fail");
        assert!(
            missing_root_error
                .to_string()
                .contains("failed to access attachment directory")
        );

        let invalid_metadata_error = AttachmentMaterializer::new(attachments_dir.clone())
            .materialize(vec![json!({ "type": "image" })])
            .await
            .expect_err("incomplete metadata must fail");
        assert!(
            invalid_metadata_error
                .to_string()
                .contains("invalid attachment metadata")
        );

        let mismatched_attachment_error = AttachmentMaterializer::new(attachments_dir.clone())
            .materialize(vec![json!({
                "type": "file",
                "id": "image-1",
                "name": "notes.txt",
                "mimeType": "image/png",
                "sizeBytes": 11
            })])
            .await
            .expect_err("mismatched type must fail");
        assert!(
            mismatched_attachment_error
                .to_string()
                .contains("type and MIME type do not match")
        );

        let missing_attachment_error = AttachmentMaterializer::new(attachments_dir.clone())
            .materialize(vec![json!({
                "type": "image",
                "id": "missing",
                "name": "missing.png",
                "mimeType": "image/png",
                "sizeBytes": 0
            })])
            .await
            .expect_err("a missing attachment must fail");
        assert!(
            missing_attachment_error
                .to_string()
                .contains("failed to read attachment")
        );

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                state.path().join("outside"),
                attachments_dir.join("linked-image"),
            )
            .expect("attachment symlink");
            let symlink_error = AttachmentMaterializer::new(attachments_dir.clone())
                .materialize(vec![json!({
                        "type": "image",
                        "id": "linked-image",
                        "name": "outside.png",
                        "mimeType": "image/png",
                        "sizeBytes": 6
                })])
                .await
                .expect_err("a symlink outside the attachment directory must fail");
            assert!(symlink_error.to_string().contains("resolves outside"));

            let materializer = AttachmentMaterializer::new(attachments_dir.clone());
            materializer
                .prepare(
                    vec![json!({
                        "type": "image",
                        "id": "linked-image",
                        "name": "outside.png",
                        "mimeType": "image/png",
                        "sizeBytes": 6,
                        "dataUrl": "data:image/png;base64,c2VjcmV0"
                    })],
                    &ReusableAttachments::new(),
                    &inline_upload_owner(),
                    &inline_upload_registry(),
                )
                .await
                .expect_err("publishing over a symlink must fail");
            let mut entries = tokio::fs::read_dir(&attachments_dir)
                .await
                .expect("attachments directory");
            while let Some(entry) = entries.next_entry().await.expect("attachment entry") {
                assert!(
                    !entry.file_name().to_string_lossy().ends_with(".upload"),
                    "failed publication must remove its staged file"
                );
            }
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn rejects_a_junction_used_as_the_attachment_root() {
        let state = TempDir::new().expect("state dir");
        let junction_target = state.path().join("junction-target");
        let attachments_dir = state.path().join("attachments");
        std::fs::create_dir(&junction_target).expect("junction target");
        if let Err(error) = junction::create(&junction_target, &attachments_dir) {
            let access_denied = matches!(error.raw_os_error(), Some(5 | 1314));
            assert!(
                access_denied,
                "junction creation failed for an unexpected reason: {error}"
            );
            eprintln!("skipping junction assertion: Windows denied junction creation: {error}");
            return;
        }

        let error = AttachmentMaterializer::new(attachments_dir)
            .prepare(
                vec![json!({
                    "type": "file",
                    "id": "notes-1",
                    "name": "notes.txt",
                    "mimeType": "text/plain",
                    "sizeBytes": 5,
                    "dataUrl": "data:text/plain;base64,bm90ZXM="
                })],
                &ReusableAttachments::new(),
                &inline_upload_owner(),
                &inline_upload_registry(),
            )
            .await
            .expect_err("a junction root must fail before publication");
        assert!(error.to_string().contains("resolves outside"));
        assert!(!junction_target.join("notes-1").exists());
    }

    #[tokio::test]
    async fn rejects_untrusted_upload_bodies_before_writing() {
        let state = TempDir::new().expect("state dir");
        let attachments_dir = state.path().join("attachments");
        let materializer = AttachmentMaterializer::new(attachments_dir.clone());
        let invalid = [
            json!({"type":"file","id":"notes-1","name":"notes.txt","mimeType":"text/plain","sizeBytes":4,"dataUrl":"data:text/plain;base64,bm90ZXM="}),
            json!({"type":"file","id":"notes-2","name":"notes.txt","mimeType":"text/plain","sizeBytes":5,"dataUrl":"data:text/plain,notes"}),
            json!({"type":"image","id":"notes-3","name":"notes.txt","mimeType":"text/plain","sizeBytes":5,"dataUrl":"data:text/plain;base64,bm90ZXM="}),
            json!({"type":"file","id":"../notes","name":"notes.txt","mimeType":"text/plain","sizeBytes":5,"dataUrl":"data:text/plain;base64,bm90ZXM="}),
            json!({"type":"file","id":"notes-4","name":"../notes.txt","mimeType":"text/plain","sizeBytes":5,"dataUrl":"data:text/plain;base64,bm90ZXM="}),
        ];
        for upload in invalid {
            materializer
                .prepare(
                    vec![upload],
                    &ReusableAttachments::new(),
                    &inline_upload_owner(),
                    &inline_upload_registry(),
                )
                .await
                .expect_err("invalid upload must fail");
        }
        assert!(!attachments_dir.join("notes-1").exists());
    }

    #[tokio::test]
    async fn attachment_metadata_matches_wire_name_and_mime_boundaries() {
        let state = TempDir::new().expect("state dir");
        let materializer = AttachmentMaterializer::new(state.path().join("attachments"));
        let max_name = "é".repeat(super::MAX_ATTACHMENT_NAME_LENGTH);
        let max_mime = format!("a/{}", "b".repeat(98));

        materializer
            .prepare(
                vec![json!({
                    "type":"file", "id":"valid-boundary", "name":max_name,
                    "mimeType":max_mime, "sizeBytes":0,
                    "dataUrl":format!("data:{max_mime};base64,")
                })],
                &ReusableAttachments::new(),
                &inline_upload_owner(),
                &inline_upload_registry(),
            )
            .await
            .expect("contract maxima prepare")
            .commit();

        for (index, (name, mime_type)) in [
            (" ".to_owned(), "text/plain".to_owned()),
            (" notes.txt".to_owned(), "text/plain".to_owned()),
            ("notes.txt ".to_owned(), "text/plain".to_owned()),
            (
                "x".repeat(super::MAX_ATTACHMENT_NAME_LENGTH + 1),
                "text/plain".to_owned(),
            ),
            ("😀".repeat(128), "text/plain".to_owned()),
            ("notes.txt".to_owned(), " ".to_owned()),
            ("notes.txt".to_owned(), " text/plain".to_owned()),
            ("notes.txt".to_owned(), format!("a/{}", "b".repeat(99))),
        ]
        .into_iter()
        .enumerate()
        {
            materializer
                .prepare(
                    vec![json!({
                        "type":"file", "id":format!("invalid-boundary-{index}"), "name":name,
                        "mimeType":mime_type, "sizeBytes":0,
                        "dataUrl":format!("data:{mime_type};base64,")
                    })],
                    &ReusableAttachments::new(),
                    &inline_upload_owner(),
                    &inline_upload_registry(),
                )
                .await
                .expect_err("metadata outside the wire contract rejects");
        }
    }

    #[tokio::test]
    async fn rejects_predecode_limits_reserved_ids_and_residual_null_data_urls() {
        let state = TempDir::new().expect("state dir");
        let materializer = AttachmentMaterializer::new(state.path().join("attachments"));
        let body = "A".repeat(super::MAX_ENCODED_ATTACHMENT_BYTES + 4);
        let error = materializer
            .prepare(vec![json!({"type":"file","id":"notes-1","name":"notes.txt","mimeType":"text/plain","sizeBytes":1,"dataUrl":format!("data:text/plain;base64,{body}")})], &ReusableAttachments::new(), &inline_upload_owner(), &inline_upload_registry())
            .await
            .expect_err("encoded body rejects before decode");
        assert_eq!(
            error.to_string(),
            "invalid attachment metadata: attachment dataUrl exceeds the base64 size limit"
        );
        for id in ["CON", "nul", &"a".repeat(129)] {
            materializer
                .prepare(vec![json!({"type":"file","id":id,"name":"notes.txt","mimeType":"text/plain","sizeBytes":0,"dataUrl":"data:text/plain;base64,"})], &ReusableAttachments::new(), &inline_upload_owner(), &inline_upload_registry())
                .await
                .expect_err("invalid Windows-safe id rejects");
        }
        materializer
            .prepare(vec![json!({"type":"image","id":"image-1","name":"screen.png","mimeType":"IMAGE/PNG","sizeBytes":0,"dataUrl":"data:IMAGE/PNG;base64,"})], &ReusableAttachments::new(), &inline_upload_owner(), &inline_upload_registry())
            .await
            .expect("image MIME matching is ASCII-case-insensitive")
            .commit();
        let reconnect = materializer
            .prepare(vec![json!({"type":"image","id":"image-1","name":"screen.png","mimeType":"image/png","sizeBytes":0})], &reusable(&["image-1"]), &inline_upload_owner(), &inline_upload_registry())
            .await
            .expect("a missing dataUrl reconnects to the prepared file");
        assert_eq!(reconnect.attachments()[0]["id"], "image-1");
        reconnect.commit();
        let null_error = materializer
            .prepare(vec![json!({"type":"image","id":"image-1","name":"screen.png","mimeType":"image/png","sizeBytes":0,"dataUrl":null})], &ReusableAttachments::new(), &inline_upload_owner(), &inline_upload_registry())
            .await
            .expect_err("an explicit null dataUrl is not a reconnect");
        assert!(
            null_error
                .to_string()
                .contains("base64 string when present")
        );
        materializer
            .materialize(vec![json!({"type":"image","id":"image-1","name":"screen.png","mimeType":"image/png","sizeBytes":0,"dataUrl":null})])
            .await
            .expect_err("residual null dataUrl rejects");
        let attachments = (0..9).map(|index| json!({"type":"file","id":format!("notes-{index}"),"name":"notes.txt","mimeType":"text/plain","sizeBytes":0,"dataUrl":"data:text/plain;base64,"})).collect();
        materializer
            .prepare(
                attachments,
                &ReusableAttachments::new(),
                &inline_upload_owner(),
                &inline_upload_registry(),
            )
            .await
            .expect_err("more than eight attachments rejects");
    }

    #[test]
    fn attached_file_section_does_not_cap_the_prompt_or_accept_control_paths() {
        let file = super::MaterializedAttachment {
            attachment_type: "file".to_owned(),
            name: "notes.txt".to_owned(),
            mime_type: "text/plain".to_owned(),
            base64_data: String::new(),
            file_url: "file:///notes".to_owned(),
            path: PathBuf::from("/safe/notes"),
        };
        let prompt = "x".repeat(32 * 1024);
        let appended = super::append_file_references(prompt.clone(), std::slice::from_ref(&file))
            .expect("generated references do not cap the prompt");
        assert!(appended.starts_with(&prompt));
        assert!(appended.contains("<attached_files>"));
        assert!(appended.contains("notes.txt: "));
        let oversized_section = super::MaterializedAttachment {
            path: PathBuf::from("x".repeat(super::MAX_ATTACHED_FILES_TEXT_BYTES)),
            ..file.clone()
        };
        assert_eq!(
            super::append_file_references(String::new(), &[oversized_section])
                .expect_err("the generated section itself remains bounded")
                .to_string(),
            "invalid attachment metadata: attached file references exceed 16 KiB"
        );
        let control = super::MaterializedAttachment {
            path: PathBuf::from("/safe/notes\nnext"),
            ..file
        };
        assert!(super::append_file_references(String::new(), &[control]).is_err());
    }

    /// The ids a turn sends without bytes are checked before anything is looked up: the batch
    /// keeps the attachment cap, every id is well formed, and each id is looked up once.
    #[test]
    fn id_only_references_are_capped_deduplicated_and_validated_before_any_lookup() {
        let reference = |id: &str| json!({"type":"file", "id":id, "name":"notes.txt", "mimeType":"text/plain", "sizeBytes":5});
        let mut upload = reference("upload-1");
        upload["dataUrl"] = json!("data:text/plain;base64,bm90ZXM=");
        assert_eq!(
            super::id_only_attachment_ids(&[
                reference("notes-1"),
                upload,
                reference("notes-1"),
                reference("notes-2"),
            ])
            .expect("ids"),
            ["notes-1", "notes-2"]
        );
        let too_many = (0..9)
            .map(|index| reference(&format!("notes-{index}")))
            .collect::<Vec<_>>();
        assert!(matches!(
            super::id_only_attachment_ids(&too_many),
            Err(AttachmentMaterializationError::InvalidMetadata(ref message))
                if message == "at most eight attachments are allowed"
        ));
        assert!(matches!(
            super::id_only_attachment_ids(&[reference("../escape")]),
            Err(AttachmentMaterializationError::InvalidId(ref id)) if id == "../escape"
        ));
    }

    #[tokio::test]
    async fn an_attachment_sent_by_id_must_be_reusable_and_unchanged() {
        let state = TempDir::new().expect("state dir");
        let materializer = AttachmentMaterializer::new(state.path().join("attachments"));
        materializer
            .prepare(
                vec![json!({
                    "type":"file", "id":"notes-1", "name":"notes.txt", "mimeType":"text/plain",
                    "sizeBytes":5, "dataUrl":"data:text/plain;base64,bm90ZXM="
                })],
                &ReusableAttachments::new(),
                &inline_upload_owner(),
                &inline_upload_registry(),
            )
            .await
            .expect("upload prepares")
            .commit();
        let reference = json!({
            "type":"file", "id":"notes-1", "name":"notes.txt", "mimeType":"text/plain", "sizeBytes":5
        });

        let refused = materializer
            .prepare(
                vec![reference.clone()],
                &ReusableAttachments::new(),
                &inline_upload_owner(),
                &inline_upload_registry(),
            )
            .await
            .expect_err("an existing file the caller may not reuse is refused");
        assert!(
            matches!(refused, AttachmentMaterializationError::NotReusable(ref id) if id == "notes-1"),
            "{refused}"
        );

        let digest = crate::crypto::sha256_hex(b"notes");
        let reused = materializer
            .prepare(
                vec![reference.clone()],
                &ReusableAttachments::from([("notes-1".to_owned(), Some(digest.clone()))]),
                &inline_upload_owner(),
                &inline_upload_registry(),
            )
            .await
            .expect("a reusable id sends the stored file again");
        assert_eq!(
            reused.references()[0].content_digest.as_deref(),
            Some(digest.as_str())
        );
        reused.commit();
        materializer
            .prepare(
                vec![reference.clone()],
                &reusable(&["notes-1"]),
                &inline_upload_owner(),
                &inline_upload_registry(),
            )
            .await
            .expect("a reference without a recorded digest is checked by size")
            .commit();
        materializer
            .prepare(
                vec![reference],
                &ReusableAttachments::from([(
                    "notes-1".to_owned(),
                    Some(crate::crypto::sha256_hex(b"other")),
                )]),
                &inline_upload_owner(),
                &inline_upload_registry(),
            )
            .await
            .expect_err("a stored file that differs from the recorded digest is refused");
    }

    #[tokio::test]
    async fn rejects_oversized_decoded_upload_and_conflicting_retries() {
        let state = TempDir::new().expect("state dir");
        let materializer = AttachmentMaterializer::new(state.path().join("attachments"));
        let oversized = STANDARD.encode(vec![0; super::MAX_ATTACHMENT_BYTES + 1]);
        materializer
            .prepare(
                vec![json!({
                    "type":"file", "id":"large-1", "name":"large.txt", "mimeType":"text/plain",
                    "sizeBytes":1, "dataUrl":format!("data:text/plain;base64,{oversized}")
                })],
                &ReusableAttachments::new(),
                &inline_upload_owner(),
                &inline_upload_registry(),
            )
            .await
            .expect_err("decoded size is capped independently of the claim");

        let upload = json!({
            "type":"file", "id":"notes-1", "name":"notes.txt", "mimeType":"text/plain",
            "sizeBytes":5, "dataUrl":"data:text/plain;base64,bm90ZXM="
        });
        materializer
            .prepare(
                vec![upload.clone()],
                &ReusableAttachments::new(),
                &inline_upload_owner(),
                &inline_upload_registry(),
            )
            .await
            .expect("initial upload prepares")
            .commit();
        materializer
            .prepare(
                vec![upload],
                &ReusableAttachments::new(),
                &inline_upload_owner(),
                &inline_upload_registry(),
            )
            .await
            .expect("identical retry prepares")
            .commit();
        materializer
            .prepare(
                vec![json!({
                    "type":"file", "id":"notes-1", "name":"notes.txt", "mimeType":"text/plain",
                    "sizeBytes":5, "dataUrl":"data:text/plain;base64,b3RoZXI="
                })],
                &ReusableAttachments::new(),
                &inline_upload_owner(),
                &inline_upload_registry(),
            )
            .await
            .expect_err("different retry cannot overwrite");
        assert_eq!(
            tokio::fs::read(state.path().join("attachments/notes-1"))
                .await
                .expect("prepared file"),
            b"notes"
        );
    }

    #[tokio::test]
    async fn serialized_publication_keeps_equal_adopters_safe_when_the_owner_rolls_back() {
        let state = TempDir::new().expect("state dir");
        let materializer = AttachmentMaterializer::new(state.path().join("attachments"));
        let adopter = materializer.clone();
        let pause = Arc::new(super::AttachmentPrepareTestPause::default());
        let owner = materializer
            .clone()
            .with_pause_after_final_publication(pause.clone());
        let upload = json!({"type":"file","id":"same-1","name":"notes.txt","mimeType":"text/plain","sizeBytes":5,"dataUrl":"data:text/plain;base64,bm90ZXM="});
        let owner = tokio::spawn({
            let upload = upload.clone();
            async move {
                owner.prepare(vec![
                    upload.clone(),
                    json!({"type":"file","id":"missing","name":"missing.txt","mimeType":"text/plain","sizeBytes":0}),
                ], &reusable(&["missing"]), &inline_upload_owner(), &inline_upload_registry())
                .await
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), pause.reached.notified())
            .await
            .expect("the owner publishes its first item");
        let adopted = tokio::spawn(async move {
            adopter
                .prepare(
                    vec![upload],
                    &ReusableAttachments::new(),
                    &inline_upload_owner(),
                    &inline_upload_registry(),
                )
                .await
        });
        tokio::task::yield_now().await;
        assert!(
            !adopted.is_finished(),
            "an equal adopter waits while the owner can still roll back"
        );
        pause.resume.notify_one();
        let failed_owner = owner.await.expect("owner task");
        let adopted = adopted.await.expect("adopter task");
        assert!(
            failed_owner.is_err(),
            "the publishing owner must fail its later item"
        );
        let adopted = adopted.expect("the equal adopter republishes after rollback");
        assert_eq!(
            tokio::fs::read(state.path().join("attachments/same-1"))
                .await
                .expect("adopted attachment remains"),
            b"notes"
        );
        adopted.commit();

        let different = json!({"type":"file","id":"different-1","name":"notes.txt","mimeType":"text/plain","sizeBytes":5,"dataUrl":"data:text/plain;base64,b3RoZXI="});
        let original = json!({"type":"file","id":"different-1","name":"notes.txt","mimeType":"text/plain","sizeBytes":5,"dataUrl":"data:text/plain;base64,bm90ZXM="});
        materializer
            .prepare(
                vec![original],
                &ReusableAttachments::new(),
                &inline_upload_owner(),
                &inline_upload_registry(),
            )
            .await
            .expect("first body publishes")
            .commit();
        materializer
            .prepare(
                vec![different],
                &ReusableAttachments::new(),
                &inline_upload_owner(),
                &inline_upload_registry(),
            )
            .await
            .expect_err("a different body cannot adopt the published id");

        materializer
            .prepare(vec![
                json!({"type":"file","id":"cleanup-1","name":"notes.txt","mimeType":"text/plain","sizeBytes":5,"dataUrl":"data:text/plain;base64,bm90ZXM="}),
                json!({"type":"file","id":"cleanup-2","name":"notes.txt","mimeType":"text/plain","sizeBytes":4,"dataUrl":"data:text/plain;base64,bm90ZXM="}),
            ], &ReusableAttachments::new(), &inline_upload_owner(), &inline_upload_registry())
            .await
            .expect_err("later batch failure cleans earlier publication");
        assert!(!state.path().join("attachments/cleanup-1").exists());
        let mut entries = tokio::fs::read_dir(state.path().join("attachments"))
            .await
            .expect("attachments");
        while let Some(entry) = entries.next_entry().await.expect("entry") {
            assert!(!entry.file_name().to_string_lossy().contains("cleanup"));
        }

        materializer
            .prepare(vec![
                json!({"type":"file","id":"rollback-1","name":"notes.txt","mimeType":"text/plain","sizeBytes":5,"dataUrl":"data:text/plain;base64,bm90ZXM="}),
                json!({"type":"file","id":"missing","name":"missing.txt","mimeType":"text/plain","sizeBytes":0}),
            ], &reusable(&["missing"]), &inline_upload_owner(), &inline_upload_registry())
            .await
            .expect_err("a later reconnect failure rolls back earlier publication");
        assert!(
            !state.path().join("attachments/rollback-1").exists(),
            "all newly published batch files must roll back"
        );
    }

    #[tokio::test]
    async fn initialization_scavenges_stages_and_cancelled_preparation_rolls_back() {
        let state = TempDir::new().expect("state dir");
        let attachments_dir = state.path().join("attachments");
        tokio::fs::create_dir(&attachments_dir)
            .await
            .expect("attachments");
        let abandoned = attachments_dir.join(".abandoned.upload");
        tokio::fs::write(&abandoned, b"partial")
            .await
            .expect("stale stage");
        let materializer = AttachmentMaterializer::new(attachments_dir.clone());
        materializer
            .prepare(
                vec![json!({
                    "type":"file", "id":"initialized", "name":"notes.txt", "mimeType":"text/plain",
                    "sizeBytes":0, "dataUrl":"data:text/plain;base64,"
                })],
                &ReusableAttachments::new(),
                &inline_upload_owner(),
                &inline_upload_registry(),
            )
            .await
            .expect("initial batch prepares")
            .commit();
        assert!(
            !abandoned.exists(),
            "one-time root initialization scavenges stages"
        );

        let body = STANDARD.encode(vec![0; super::MAX_ATTACHMENT_BYTES]);
        let cancelled_path = attachments_dir.join("cancelled");
        let task_materializer = materializer.clone();
        let task = tokio::spawn(async move {
            task_materializer
                .prepare(vec![json!({
                    "type":"file", "id":"cancelled", "name":"large.bin",
                    "mimeType":"application/octet-stream", "sizeBytes":super::MAX_ATTACHMENT_BYTES,
                    "dataUrl":format!("data:application/octet-stream;base64,{body}")
                })], &ReusableAttachments::new(), &inline_upload_owner(), &inline_upload_registry())
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let has_stage_or_final = cancelled_path.exists()
                    || std::fs::read_dir(&attachments_dir)
                        .expect("attachment entries")
                        .flatten()
                        .any(|entry| entry.file_name().to_string_lossy().ends_with(".upload"));
                if has_stage_or_final {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("preparation reaches owned filesystem state");
        task.abort();
        let _ = task.await;
        assert!(!cancelled_path.exists(), "cancelled final is rolled back");
        assert!(
            std::fs::read_dir(&attachments_dir)
                .expect("attachment entries")
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().ends_with(".upload")),
            "cancelled stages are removed synchronously"
        );
    }

    #[test]
    fn prompt_parts_preserve_text_and_attachment_only_turns() {
        let image = json!({ "type": "image", "data": "aW1hZ2U=", "mimeType": "image/png" });

        assert_eq!(
            super::prompt_parts(Some("describe this"), vec![image.clone()]),
            vec![
                json!({ "type": "text", "text": "describe this" }),
                image.clone()
            ]
        );
        assert_eq!(
            super::prompt_parts(Some(""), vec![image.clone()]),
            vec![image]
        );
    }
    use crate::transfer::staging::{
        UploadAppendInput, UploadBeginInput, UploadErrorReason, UploadLimits,
    };
    fn upload_fixture() -> (
        tempfile::TempDir,
        UploadRegistry,
        AttachmentMaterializer,
        UploadOwner,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let uploads = UploadRegistry::new(
            temp.path().join("attachment-uploads"),
            UploadLimits::default(),
            Arc::new(tokio::time::Instant::now),
        );
        let materializer = AttachmentMaterializer::new(temp.path().join("attachments"));
        (
            temp,
            uploads,
            materializer,
            UploadOwner::Session("a".into()),
        )
    }
    async fn stage_notes(uploads: &UploadRegistry, owner: &UploadOwner, complete: bool) -> Value {
        let begun = uploads
            .begin(
                owner,
                UploadBeginInput {
                    target: UploadTarget::ChatAttachment {
                        attachment_type: "file".into(),
                        name: "notes.txt".into(),
                        mime_type: "text/plain".into(),
                    },
                    size_bytes: 5,
                    sha256: None,
                },
            )
            .await
            .unwrap();
        if complete {
            uploads
                .append(
                    owner,
                    UploadAppendInput {
                        upload_id: begun.upload_id.clone(),
                        offset: 0,
                        data: STANDARD.encode(b"notes"),
                        sha256: Some(crate::crypto::sha256_hex(b"notes")),
                    },
                )
                .await
                .unwrap();
        }
        json!({"type":"file", "id":"notes-1", "name":"notes.txt", "mimeType":"text/plain",
        "sizeBytes":5, "uploadId":begun.upload_id})
    }
    #[tokio::test]
    async fn staged_binding_rolls_back_without_consuming_bytes_and_commit_releases_them() {
        let (temp, uploads, m, owner) = upload_fixture();
        let input = stage_notes(&uploads, &owner, true).await;
        let id = input["uploadId"].as_str().unwrap();
        let batch = m
            .prepare(
                vec![input.clone()],
                &ReusableAttachments::new(),
                &owner,
                &uploads,
            )
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::read(temp.path().join("attachments/notes-1"))
                .await
                .unwrap(),
            b"notes"
        );
        assert!(batch.attachments()[0].get("uploadId").is_none());
        assert!(batch.attachments()[0].get("dataUrl").is_none());
        drop(batch);
        assert!(!temp.path().join("attachments/notes-1").exists());
        assert!(uploads.get(&owner, id).await.unwrap().complete);
        let batch = m
            .prepare(
                vec![input.clone()],
                &ReusableAttachments::new(),
                &owner,
                &uploads,
            )
            .await
            .unwrap();
        let prepared = batch.attachments().to_vec();
        batch.commit();
        assert_eq!(
            uploads.get(&owner, id).await.unwrap_err().reason,
            UploadErrorReason::NotFound
        );
        assert!(
            !temp
                .path()
                .join("attachment-uploads")
                .join(format!("{id}.upload"))
                .exists()
        );
        let delivered = m.materialize(prepared).await.unwrap();
        assert_eq!(
            STANDARD.decode(&delivered[0].base64_data).unwrap(),
            b"notes"
        );
    }
    #[tokio::test]
    async fn staged_binding_refuses_incomplete_foreign_and_changed_metadata() {
        let (_temp, uploads, m, owner) = upload_fixture();
        let unfinished = stage_notes(&uploads, &owner, false).await;
        assert!(
            m.prepare(
                vec![unfinished],
                &ReusableAttachments::new(),
                &owner,
                &uploads
            )
            .await
            .is_err()
        );
        let input = stage_notes(&uploads, &owner, true).await;
        let other = UploadOwner::Session("other".into());
        assert!(
            m.prepare(
                vec![input.clone()],
                &ReusableAttachments::new(),
                &other,
                &uploads
            )
            .await
            .is_err()
        );
        for (field, value) in [
            ("sizeBytes", json!(4)),
            ("name", json!("other.txt")),
            ("mimeType", json!("application/octet-stream")),
            ("type", json!("image")),
        ] {
            let mut changed = input.clone();
            changed[field] = value;
            assert!(
                m.prepare(vec![changed], &ReusableAttachments::new(), &owner, &uploads)
                    .await
                    .is_err()
            );
        }
        assert!(
            uploads
                .get(&owner, input["uploadId"].as_str().unwrap())
                .await
                .unwrap()
                .complete
        );
    }
    #[tokio::test]
    async fn staged_binding_refuses_null_or_two_sources_before_the_reuse_branch() {
        let (_temp, uploads, m, owner) = upload_fixture();
        let input = stage_notes(&uploads, &owner, true).await;
        for source in [json!(null), json!("data:text/plain;base64,bm90ZXM=")] {
            let mut ambiguous = input.clone();
            ambiguous["dataUrl"] = source;
            assert!(
                m.prepare(
                    vec![ambiguous],
                    &ReusableAttachments::new(),
                    &owner,
                    &uploads
                )
                .await
                .is_err()
            );
        }
        let mut invalid = input.clone();
        invalid["uploadId"] = Value::Null;
        assert!(
            m.prepare(vec![invalid], &ReusableAttachments::new(), &owner, &uploads)
                .await
                .is_err()
        );
        assert!(
            m.prepare(vec![input], &ReusableAttachments::new(), &owner, &uploads)
                .await
                .is_ok()
        );
    }
    #[tokio::test]
    async fn copy_fallback_and_adopted_finals_preserve_rollback_ownership() {
        let (temp, uploads, materializer, owner) = upload_fixture();
        let m = materializer.with_forced_copy_for_test();
        let first = stage_notes(&uploads, &owner, true).await;
        m.prepare(vec![first], &ReusableAttachments::new(), &owner, &uploads)
            .await
            .unwrap()
            .commit();
        let second = stage_notes(&uploads, &owner, true).await;
        let batch = m
            .prepare(
                vec![second.clone()],
                &ReusableAttachments::new(),
                &owner,
                &uploads,
            )
            .await
            .unwrap();
        drop(batch); // identical pre-existing final was adopted, not created by this batch
        assert_eq!(
            tokio::fs::read(temp.path().join("attachments/notes-1"))
                .await
                .unwrap(),
            b"notes"
        );
        assert!(
            uploads
                .get(&owner, second["uploadId"].as_str().unwrap())
                .await
                .unwrap()
                .complete
        );
        tokio::fs::write(temp.path().join("attachments/notes-1"), b"other")
            .await
            .unwrap();
        assert!(
            m.prepare(vec![second], &ReusableAttachments::new(), &owner, &uploads)
                .await
                .is_err()
        );
        assert_eq!(
            tokio::fs::read(temp.path().join("attachments/notes-1"))
                .await
                .unwrap(),
            b"other"
        );
    }
    #[tokio::test(start_paused = true)]
    async fn a_bound_upload_cannot_expire_out_from_under_a_committing_batch() {
        let (temp, uploads, m, owner) = upload_fixture();
        let input = stage_notes(&uploads, &owner, true).await;
        let id = input["uploadId"].as_str().unwrap().to_owned();
        let batch = m
            .prepare(vec![input], &ReusableAttachments::new(), &owner, &uploads)
            .await
            .unwrap();
        tokio::time::advance(Duration::from_secs(601)).await;
        let sweep_registry = uploads.clone();
        let sweep = tokio::spawn(async move { sweep_registry.sweep().await });
        tokio::task::yield_now().await;
        batch.commit(); // sweep may skip a locked binding or wait; it cannot delete its final
        sweep.await.unwrap().unwrap();
        assert_eq!(
            tokio::fs::read(temp.path().join("attachments/notes-1"))
                .await
                .unwrap(),
            b"notes"
        );
        assert_eq!(
            uploads.get(&owner, &id).await.unwrap_err().reason,
            UploadErrorReason::NotFound
        );
    }
    #[tokio::test]
    async fn staged_duplicate_ids_fail_without_deadlock_or_consuming_the_upload() {
        let (_temp, uploads, m, owner) = upload_fixture();
        let input = stage_notes(&uploads, &owner, true).await;
        let id = input["uploadId"].as_str().unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            m.prepare(
                vec![input.clone(), input.clone()],
                &ReusableAttachments::new(),
                &owner,
                &uploads,
            ),
        )
        .await
        .unwrap();
        assert!(result.is_err());
        assert!(uploads.get(&owner, id).await.unwrap().complete);
    }
    #[tokio::test]
    async fn staged_commit_fences_cancel_and_preserves_the_durable_final() {
        let (temp, uploads, m, owner) = upload_fixture();
        let input = stage_notes(&uploads, &owner, true).await;
        let id = input["uploadId"].as_str().unwrap().to_owned();
        let batch = m
            .prepare(vec![input], &ReusableAttachments::new(), &owner, &uploads)
            .await
            .unwrap();
        let cancel_registry = uploads.clone();
        let cancel_owner = owner.clone();
        let cancel_id = id.clone();
        let cancel =
            tokio::spawn(async move { cancel_registry.cancel(&cancel_owner, &cancel_id).await });
        tokio::task::yield_now().await;
        assert!(!cancel.is_finished());
        batch.commit();
        cancel.await.unwrap().unwrap();
        assert_eq!(
            std::fs::read(temp.path().join("attachments/notes-1")).unwrap(),
            b"notes"
        );
        assert_eq!(
            uploads.get(&owner, &id).await.unwrap_err().reason,
            UploadErrorReason::NotFound
        );
    }
    #[tokio::test]
    async fn canceled_blocking_publication_cannot_erase_a_later_adopters_final() {
        let (temp, uploads, materializer, owner) = upload_fixture();
        let materializer = materializer.with_forced_copy_for_test();
        let input = stage_notes(&uploads, &owner, true).await;
        let pause = Arc::new(super::AttachmentPrepareTestPause::default());
        let path = temp
            .path()
            .canonicalize()
            .unwrap()
            .join("attachments/notes-1");
        struct ResumeOnDrop(Arc<super::AttachmentPrepareTestPause>);
        impl Drop for ResumeOnDrop {
            fn drop(&mut self) {
                self.0.resume.notify_one();
            }
        }
        let _resume_on_drop = ResumeOnDrop(pause.clone());
        super::PUBLICATION_PAUSES
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .insert(path.clone(), pause.clone());
        let first_m = materializer.clone();
        let first_u = uploads.clone();
        let first_o = owner.clone();
        let first_input = input.clone();
        let first = tokio::spawn(async move {
            first_m
                .prepare(
                    vec![first_input],
                    &ReusableAttachments::new(),
                    &first_o,
                    &first_u,
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), pause.reached.notified())
            .await
            .expect("publication pause reached");
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        // The blocking job retains both root and upload locks until its owned
        // final has been removed, even after its RPC future disappears.
        assert!(materializer.root_transaction.try_lock().is_err());
        let second_m = materializer.clone();
        let second_u = uploads.clone();
        let second_o = owner.clone();
        let second = tokio::spawn(async move {
            second_m
                .prepare(
                    vec![input],
                    &ReusableAttachments::new(),
                    &second_o,
                    &second_u,
                )
                .await
        });
        tokio::task::yield_now().await;
        assert!(!second.is_finished());
        super::PUBLICATION_PAUSES
            .get()
            .unwrap()
            .lock()
            .unwrap()
            .remove(&path);
        pause.resume.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(5), second).await;

        result
            .expect("later adopter completes after canceled publication cleanup")
            .unwrap()
            .unwrap()
            .commit();
        assert_eq!(std::fs::read(path).unwrap(), b"notes");
    }
    #[tokio::test]
    async fn staged_image_admission_preserves_native_image_bytes_and_durable_metadata() {
        let (_temp, uploads, m, owner) = upload_fixture();
        let bytes = STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aN5kAAAAASUVORK5CYII=").unwrap();
        let begun = uploads
            .begin(
                &owner,
                UploadBeginInput {
                    target: UploadTarget::ChatAttachment {
                        attachment_type: "image".into(),
                        name: "pixel.png".into(),
                        mime_type: "image/png".into(),
                    },
                    size_bytes: bytes.len() as u64,
                    sha256: None,
                },
            )
            .await
            .unwrap();
        uploads
            .append(
                &owner,
                UploadAppendInput {
                    upload_id: begun.upload_id.clone(),
                    offset: 0,
                    data: STANDARD.encode(&bytes),
                    sha256: Some(crate::crypto::sha256_hex(&bytes)),
                },
            )
            .await
            .unwrap();
        let batch = m.prepare(vec![json!({"type":"image","id":"pixel-1","name":"pixel.png","mimeType":"image/png","sizeBytes":bytes.len(),"uploadId":begun.upload_id})], &ReusableAttachments::new(), &owner, &uploads).await.unwrap();
        let prepared = batch.attachments().to_vec();
        assert!(prepared[0].get("uploadId").is_none());
        assert!(prepared[0].get("dataUrl").is_none());
        batch.commit();
        let images = m.materialize(prepared).await.unwrap();
        assert_eq!(images[0].attachment_type, "image");
        assert_eq!(STANDARD.decode(&images[0].base64_data).unwrap(), bytes);
        assert_eq!(
            uploads
                .get(&owner, &begun.upload_id)
                .await
                .unwrap_err()
                .reason,
            UploadErrorReason::NotFound
        );
    }
}

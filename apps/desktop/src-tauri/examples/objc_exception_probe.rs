#[cfg(target_os = "macos")]
fn main() {
    use objc2::exception::{Exception, catch, throw};
    use objc2::rc::Retained;
    use objc2_foundation::{NSException, NSString};
    use std::panic::AssertUnwindSafe;

    let name = NSString::from_str("CoddyStudioCancelledSchemeTaskProbe");
    let reason = NSString::from_str("cancelled URL-scheme task probe");
    // No user-info dictionary is supplied, so the Objective-C generic contract
    // is satisfied. Exception can wrap any Objective-C exception object.
    let exception =
        unsafe { NSException::exceptionWithName_reason_userInfo(&name, Some(&reason), None) };
    let exception: Retained<Exception> = unsafe { Retained::cast_unchecked(exception) };
    let recovered = catch(AssertUnwindSafe(|| throw(exception)))
        .expect_err("the probe must raise an Objective-C exception")
        .expect("the probe exception must not be nil");
    assert!(
        recovered
            .to_string()
            .contains("cancelled URL-scheme task probe")
    );
    println!("Objective-C exception recovery passed in the release profile.");
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("This native exception probe requires macOS.");
    std::process::exit(1);
}

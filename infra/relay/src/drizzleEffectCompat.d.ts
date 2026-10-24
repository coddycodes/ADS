// drizzle-orm 1.0.0-rc.5-ab785fc (pinned exactly by alchemy's drizzle peer)
// was published against an Effect 4 prerelease. Its declaration files still
// import `effect/unstable/sql/SqlError`, a path Effect 4.0 stable removed when
// it dropped the `unstable` segment. Its runtime JavaScript never imports that
// path; only the types do. Without this shim the unresolved import collapses
// drizzle's `SqlError` to `any`, which leaks into every transaction's error
// channel. Re-export the stable module under the legacy name so drizzle's
// declarations keep their typed error. Remove once drizzle's types use
// `effect/sql/SqlError`.
declare module "effect/unstable/sql/SqlError" {
  export * from "effect/sql/SqlError";
}

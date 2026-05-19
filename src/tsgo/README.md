### Integrity with runtime project

The packages installed in the `package.json` here needs to be synced with `units/runtime-check/package.json` on the runtime project to make sure typescript is doing the same when LLM writes code here and when the types are checked in tests in runtime tests.

Also note `runtime-project-tsc-type-deps.json` is copied from the runtime project (`units/runtime-check/`). `build.rs` embeds exactly its `files` list into `node_modules_files.rs` (build fails if any path is missing).
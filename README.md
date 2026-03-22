## Preparing app settings (before running ./migration-up.dev)
```
CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT);

INSERT INTO app_settings (key, value) VALUES ('env', 'development'); # or 'production'
```

## Creating new migration file
```
diesel migration generate create_users_table
```

## Initial setup for queue (run once per environment)
```bash
./queue_setup_http.sh
```
Loads from `.env`: `RABBIT_URL`, `RABBITMQ_MANAGEMENT_URL`, `RETRY_DELAY_EXCHANGE`, `RETRY_DELAY_TTL_MS`, `CHAT_CONSUMER_TIMEOUT_SECS`. User/password are parsed from `RABBIT_URL`.

See [docs/chat-queue.md](docs/chat-queue.md) for how the chat queue, worker, WebSocket, and LLM interact.

** It doesn't use `.env.local` at all, just `.env` **

## To fix libpg issue (library not found)

[GPT Conversation](https://chatgpt.com/share/698ccc50-aea8-800e-b2d2-fb27cb695f0d)

```sh
brew install postgresql
brew install libpq
brew link --force libpq
```

```sh
cargo clean
cargo build
```

Add this to ~/.cargo/config.toml:
```toml
[env]
LIBRARY_PATH = "/opt/homebrew/opt/libpq/lib"
```
(Use `brew --prefix libpq` to get correct path)

## Resources
https://blocks.so/stats

https://ui.8starlabs.com/docs/components/timeline

https://www.abui.io/utils/debug-utils

https://elements.ai-sdk.dev/components/attachments

https://basecn.dev/docs/components/slider

https://billingsdk.com/docs/components/update-plan/update-plan-dialog
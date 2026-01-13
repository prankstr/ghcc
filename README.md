# ghcc

Fast CLI tool for generating conventional commit messages with GitHub Copilot.

## Features

- **Conventional commits** - Generates conventional commit style messages ([spec](https://www.conventionalcommits.org/))
- **Fast** - Single command, no prompts required
- **Commit styles** - Single-line, detailed body to provide context, or let the AI decide
- **Git hook integration** - Pre-fills your editor so you can check and edit before committing
- **Uses GitHub Copilot** - Just log in with your existing account. No API keys needed.
- **Model selection** - Choose from your enabled Copilot models (default: `gpt-4.1`)

> [!TIP]
> `gpt-4.1` (default) seems to perform best of the free models. For a bump in quality, any of the non-Codex GPT-5 models (`gpt-5`, `gpt-5.1`, `gpt-5.2`) perform well but will consume premium requests.

## Quickstart

1. **Install ghcc:**

   - **Option A: Download binary from releases** (recommended)

     Download the latest binary for your platform from [Releases](../../releases):

     | Platform | Binary |
     |----------|--------|
     | Linux (x64) | `ghcc-x86_64-unknown-linux-gnu` |
     | Linux (ARM64) | `ghcc-aarch64-unknown-linux-gnu` |
     | macOS (Intel) | `ghcc-x86_64-apple-darwin` |
     | macOS (Apple Silicon) | `ghcc-aarch64-apple-darwin` |

     ```sh
     install -Dm755 ghcc-x86_64-unknown-linux-gnu ~/.local/bin/ghcc
     ```

   - **Option B: Build from source**

     ```sh
     git clone https://github.com/prankstr/ghcc.git
     cd ghcc
     cargo build --release
     install -Dm755 target/release/ghcc ~/.local/bin/ghcc
     ```

2. **Login:**

   ```sh
   ghcc login
   ```

3. **Generate commit messages:**

   ```sh
   git add -p
   ghcc
   ```

## Usage

```sh
ghcc              # Single-line commit message (default)
ghcc -d           # Detailed: subject + paragraph body to provide context
ghcc -a           # Auto: AI decides format based on complexity
ghcc status       # Check authentication status
ghcc models       # List and select models (default: gpt-4.1)
ghcc hook install # Install git hook for current repo
ghcc hook uninstall
```

## Ignoring Files

Create a `.ghccignore` file in your repo root to exclude files from the diff sent to the AI:

```
# Large generated files
package-lock.json
pnpm-lock.yaml
*.lock

# Assets
**/*.png
**/*.jpg
dist/
```

Uses git pathspec syntax. Nothing is ignored by default.

## Integrations

### LazyGit

Add this to your LazyGit config (`~/.config/lazygit/config.yml`):

```yaml
customCommands:
  - key: 'G'
    context: 'files'
    description: 'AI Commit'
    loadingText: 'Generating commit message...'
    prompts:
      - type: 'input'
        title: 'Commit Message (edit or press Enter)'
        key: 'Message'
        initialValue: '{{ runCommand "ghcc" }}'
    command: 'git commit -m {{.Form.Message | quote}}'
```

Press `G` to generate, edit if needed, then press Enter.

Change `ghcc` to `ghcc -d` for detailed messages, or `ghcc -a` for auto mode. You can also use multiple keybinds for different styles.

### Git Hook

Install the hook for the current repository:

```sh
ghcc hook install      # Uses single-line format (default)
ghcc hook install -a   # Uses auto mode (AI decides)
ghcc hook install -d   # Uses detailed format
```

Now `git commit` will pre-fill the message with ghcc output. The editor opens so you can review/edit before committing. You can still override it with `-m`.

**Note:** The hook only runs for normal commits. It won't run for merges or amends.

To uninstall:

```sh
ghcc hook uninstall
```

## Requirements

- Linux or macOS (Windows is not supported)
- GitHub Copilot subscription
- Git

## License

MIT

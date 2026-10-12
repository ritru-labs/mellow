# AI

AI is optional and off until you set it up. Core editing works without it.
Explicit Ask AI requests send the context shown in the prompt. If you separately
enable inline suggestions, nearby code is sent automatically after typing
pauses. Mellow does not give the AI shell or Git execution capabilities.

These controls describe the unreleased audit candidate. Published v0.3.0
predates the shell-review and redirect protections; this implementation has not
published or upgraded any release.

## Set up

Press `Ctrl+K` (or open Settings, then AI assistant) and choose a provider:

- **Claude** (Anthropic)
- **OpenAI**
- **Gemini** (Google)
- **Ollama**, which runs on your computer and needs no key
- any **OpenAI-compatible** endpoint

Paste your key once. It is saved to `~/.config/mellow/ai.conf`, stored with owner-only permissions on supported Unix systems. The file is
plain text, not an encrypted credential vault. If you prefer not to save a key, Mellow uses `ANTHROPIC_API_KEY`,
`OPENAI_API_KEY` or `GEMINI_API_KEY` from your environment.

## Use

- `Ctrl+K` opens Ask AI next to the cursor and says exactly what will be sent,
  for example "lines 13–16 of scripts/deploy.sh to Claude. Nothing else."
- `Tab` offers Explain, Fix problems and Write tests.
- With a selection, a suggested edit appears as a diff. It is applied only when
  you accept it, as one edit you can undo.
- Optional typing suggestions (off by default) show grey text after a pause at
  the end of a line you just typed; `Tab` accepts.

## Work on a problem

Each of these is in the command palette (`Ctrl+P`), and each waits for your
review before anything changes:

- **Fix problem on this line with AI:** sends the line that has a problem, with
  its message, and shows the fix as a diff. Accept it with `Enter`, or reject it
  with `Esc`.
- **Ask AI for a shell command:** describe what you want in plain words. The
  command appears in a read-only review. `C` copies it; `Enter` closes the view.
  Mellow never inserts it into the terminal: an idle shell prompt cannot be
  reliably distinguished from a foreground program or partially typed command.
  You choose where to paste and separately submit it. Multiline replies and
  terminal controls (CR, LF, ESC, NUL and C1) are refused before review.
- **Draft a commit message:** press `Ctrl+G` in the commit box. See
  [Keys](keys.md#in-dialogs).
- **Undo last AI edit:** undoes the most recent accepted AI edit, as long as it is
  still the latest change. If you have typed since, `Ctrl+Z` steps back instead.
- **Show AI edits this session:** lists the latest accepted AI edits.

Mellow records AI edits for the current session only. They are not saved to disk,
and an AI edit touches only the file you are editing.

## Environment variables

An environment configuration takes precedence over the saved setup when
`MELLOW_AI_ENDPOINT` is set; `MELLOW_AI_MODEL` is then required. Choose a model
available from your provider. These placeholders are not runnable credentials:

```bash
export MELLOW_AI_PROVIDER="claude"     # claude | openai | gemini | ollama | custom
export MELLOW_AI_ENDPOINT="https://api.anthropic.com/v1/messages"
export MELLOW_AI_MODEL="<your-provider-model>"
export MELLOW_AI_API_KEY="..."         # not needed for local providers
export MELLOW_AI_INLINE=0              # keep automatic typing suggestions off
```

Provider API access and model availability are separate from installing Mellow.
A hosted provider may charge for requests; a local Ollama endpoint requires a
running server and an installed model. Check the displayed destination and
context before submitting. Do not paste keys into project files or bug reports.

If setup fails, check the model, endpoint and key for that provider. HTTPS is
required for remote endpoints; plain HTTP is allowed for loopback endpoints.
Provider integrations have not been validated against every live account and
model. An explanation is read-only; a proposed selection replacement must be
accepted before it changes the buffer, and saved before it changes the file.

## Privacy, limits and dismissal

The default privacy policy refuses AI context from `.env`/`.env.*` files (except
example/sample templates), private-key files, SSH/AWS directories and credential
files, including common `secrets.json`/YAML/TOML names. It also refuses text with recognizable private keys or credential
assignments. This is a conservative first boundary, not a complete secret
scanner: inspect the disclosed context yourself. Selection text is refused,
not silently redacted, so a generated replacement cannot overwrite a different
redacted selection. Staged commit diffs containing excluded file paths or
recognized credentials are refused before transport.

Instructions are limited to 8,000 characters, selections to 32,000 characters,
HTTP response envelopes to 512 KiB and replies to 64,000 characters. Providers
receive an output-token cap; a response cut off at its token limit is refused.
Redirects are disabled for every provider, and provider error bodies are
withheld because they can echo credentials or terminal controls. Remote
requests require HTTPS; HTTP is permitted only on loopback.

`Esc` aborts the local transport while waiting for headers or streamed output
and discards its result. The provider may already be processing the request or
charging for it; local cancellation is not a refund or server-side guarantee.
Streamed text is a read-only preview; only a complete, validated proposal can
reach the ordinary acceptance diff. Partial, malformed or token-truncated
proposals never change a buffer.

Use **AI request usage and limits** in the palette to inspect session attempts,
provider-reported tokens and unknown usage. Failed/cancelled attempts count.
Set `ai_max_requests`, `ai_max_output_tokens` and `ai_session_token_budget` in
[settings.conf](settings.md). The token budget is enforced from observed
provider reports after responses; unknown input usage cannot establish a hard
monetary ceiling. Prices and cost remain unknown. These are session controls,
not a persistent receipt ledger.

Add exclusions in your user-owned `~/.config/mellow/ai-deny.conf` (or
`$XDG_CONFIG_HOME/mellow/ai-deny.conf`); see the
[security evidence](audit/security-verification.md) for glob syntax and limits.
Defaults remain mandatory. Unreadable or invalid policy fails closed.
The file must not be writable by group or others. Where the default umask is
`0002` (Ubuntu, for example), a new file is group-writable, so run
`chmod 600 ~/.config/mellow/ai-deny.conf` after creating it.
Context is refused before transmission rather than silently rewritten.
Transport timeouts are 120 seconds for Ask AI/commit messages, 60 seconds for
shell suggestions and 20 seconds for inline suggestions. Live hosted-provider
account/model compatibility and server-side cancellation remain unqualified.

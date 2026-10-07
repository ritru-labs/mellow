# AI

AI is optional and off until you set it up. Mellow sends code only when you ask,
and never runs commands, Git operations or file changes on its own.

## Set up

Press `Ctrl+K` (or open Settings, then AI assistant) and choose a provider:

- **Claude** (Anthropic)
- **OpenAI**
- **Gemini** (Google)
- **Ollama**, which runs on your computer and needs no key
- any **OpenAI-compatible** endpoint

Paste your key once. It is saved to `~/.config/mellow/ai.conf`, readable only by
you. If you prefer not to save a key, Mellow uses `ANTHROPIC_API_KEY`,
`OPENAI_API_KEY` or `GEMINI_API_KEY` from your environment.

## Use

- `Ctrl+K` opens Ask AI next to the cursor and says exactly what will be sent,
  for example "lines 13–16 of scripts/deploy.sh to Claude. Nothing else."
- `Tab` offers Explain, Fix problems and Write tests.
- With a selection, a suggested edit appears as a diff. It is applied only when
  you accept it, as one edit you can undo.
- Optional typing suggestions (off by default) show grey text after a pause at
  the end of a line you just typed; `Tab` accepts.

## Environment variables

These override the saved setup:

```bash
export MELLOW_AI_PROVIDER="claude"     # claude | openai | gemini | ollama | custom
export MELLOW_AI_ENDPOINT="https://api.anthropic.com/v1/messages"
export MELLOW_AI_MODEL="claude-opus-5-5"
export MELLOW_AI_API_KEY="..."         # not needed for local providers
export MELLOW_AI_INLINE=1              # turn on typing suggestions
```

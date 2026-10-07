# AI

AI is optional and off until you set it up. Core editing works without it.
Explicit Ask AI requests send the context shown in the prompt. If you separately
enable inline suggestions, nearby code is sent automatically after typing
pauses. Mellow does not give the AI shell or Git execution capabilities.

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

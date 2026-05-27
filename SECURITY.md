# Security Policy

## Supported Versions

Hyvemind is in **alpha**. Only the latest tagged release is supported with
security fixes. Older builds will not receive backports.

## Threat Model

Hyvemind is a **local desktop application** (Tauri 2 + Rust + React). It runs
on the user's own machine, operates on a user-chosen working directory, and
delegates LLM calls to providers the user has configured. Users bring their
own API keys; keys are stored in the **OS keychain** via the `keyring` crate
(never plaintext on disk).

The Hivemind review engine and Swarm execution engine send prompt content
(including any text the user has supplied) to the configured LLM providers
over HTTPS.

### In scope

We treat the following as security-relevant and want to hear about them:

- **Secret leakage** via debug logs, config files, error messages, IPC
  payloads, telemetry, or crash dumps.
- **IPC payloads escaping intended directories** — Tauri commands accepting
  paths that allow read/write outside the user-chosen working directory.
- **Supply-chain CVEs** in Rust crates or npm packages used at runtime
  (tracked by the `Security Audit` GitHub Actions workflow).
- **Common Tauri foot-guns** — missing or overly permissive CSP, dangerous
  default capabilities, unsafe `tauri.conf.json` settings, unsafe `eval`
  or `dangerouslySetInnerHTML` on untrusted markdown.
- **Privilege escalation or sandbox escape** in the Pi subprocess wrapper
  beyond the documented working-directory model.

### Out of scope (deliberate trust assumptions)

These are accepted limitations of an autonomous coding tool:

- **Prompt injection from user-authored feature descriptions** — the user
  is the operator; their own input is trusted.
- **Hostile LLM output** — the user chooses their providers; we do not
  attempt to defend against an LLM that produces malicious code or text.
  The user is expected to review changes.
- **Working-directory escape by Pi tools** — Pi runs with the user's
  permissions inside the user-chosen directory. Tools that legitimately
  need to write outside that directory (e.g. shell commands) operate at
  the user's risk.
- **Runaway model spend** — cost guards are best-effort; the user is
  responsible for monitoring and capping their provider accounts.
- **Physical access to an unlocked machine** — keychain access falls back
  to the OS's session-unlock policy.

## Reporting a Vulnerability

If you find something in scope, please report it privately:

- **Preferred:** open a private Security Advisory on the GitHub repository.
- **Alternative:** email `xent22@gmail.com` with `[Hyvemind security]` in
  the subject line and a clear description plus reproduction steps.

We aim to triage reports within roughly **7 days**. Because Hyvemind is a
volunteer alpha project, fix timelines are best-effort. We will credit
reporters in release notes unless they request otherwise.

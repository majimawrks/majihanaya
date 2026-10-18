# Test fixtures

- `usage_pro.txt`: real `claude -p /usage` output (Claude Code 2.1.302, Pro), 2026-10-18.
- `usage_fable.txt`: real output with a per-model week, from the old widget's docs.
- `usage_one_session_no_usage.txt`, `usage_logged_out.txt`: **synthetic**, shaped
  after the real format (singular "1 session", 0% with no reset, uppercase PM).
  The logged-out text is a guess; the parser must just find nothing in it.
- `auth_status*.json`: real shape, identifiers replaced.
- `desktop_history.json`: real shape of `%APPDATA%\Claude\plan-usage-history.json`, org replaced.
- `codex_appserver_plus.jsonl`: real `codex app-server` replies (codex-cli 0.149.0), identifiers replaced.
- `codex_ratelimits_two_windows.json`, `codex_rollout_token_count.jsonl`, `status_summary_*.json`:
  **synthetic**, following the documented field names.

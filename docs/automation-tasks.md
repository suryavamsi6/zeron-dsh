# Automation tasks

Settings → Automation tasks lists device-local and edge-owned schedules together, and marks where each one fires:

- **On this device** — the local desktop engine owns the task. It fires only while the app is running. If the app is closed when a run comes due, only the latest missed occurrence fires once when the app starts again; older missed occurrences do not pile up.
- **At the edge** — the edge owns the task for a cloud Captain. Closing the desktop app does not stop it; the edge fires the occurrence and delivers the wake-up to the hosted machine.

Editing, pausing, resuming, or deleting a row writes the store that owns it. A task keeps the origin it was created with: moving a chat between desktop and cloud does not move its existing tasks.

**Run now** is available only for device-local tasks. The edge does not yet have a manual-run mutation, so edge-owned rows report that Run now is unavailable.

# Automation tasks

Settings → Automation tasks marks where each schedule fires:

- **On this device** — the local desktop engine owns the task. It fires only while the app is running. If the app is closed when a run comes due, only the latest missed occurrence fires once when the app starts again; older missed occurrences do not pile up.
- **At the edge** — the edge owns the task for a cloud Captain. Closing the desktop app does not stop it; the edge fires the occurrence and delivers the wake-up to the hosted machine.

A task keeps the origin it was created with. Moving a chat between desktop and cloud does not move its existing tasks.

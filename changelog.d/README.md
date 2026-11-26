# Changelog fragments

Crew branches must not edit the root `VERSION` or `CHANGELOG.md`: parallel branches collide on those stamped files. Add one fragment named `changelog.d/<slug>.md`, where `<slug>` is lowercase letters, digits and dashes, and let the release process fold fragments into the stamped changelog after the branches land.

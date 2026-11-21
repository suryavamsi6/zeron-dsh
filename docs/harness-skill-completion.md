# Commands and skill completion across harnesses

## Workspace commands

Every harness gets Zeron actions in the `/` picker. Selecting one runs the action
immediately; it does not create a model turn. Actions that require a conversation
are shown only in an existing chat.

| Command | Action |
| --- | --- |
| `/model` | Choose agent, model, and reasoning |
| `/new` | Start a new conversation |
| `/resume` | Search and open conversations |
| `/settings` | Open Zeron settings |
| `/diff` | Open a changes tab |
| `/files` | Open project files |
| `/terminal` | Open a terminal tab |
| `/rename` | Open the conversation rename dialog |
| `/stop` | Interrupt the current run |

DeepSeek Harness exposes no native command catalog to Zeron, so the deepseek
agent contributes no native command catalog; the Zeron actions above are the slash
picker's entries. This never changes what the agent can do in its own terminal:
only commands backed by a protocol or a Zeron action are advertised.

Any legacy harness id still decodes so historical sessions render, but it has
no runtime, no command catalog, and cannot be selected for a new run.

## Skill preferences

Settings → DeepSeek → Completion has two independent controls for the DeepSeek
agent on this device:

- **$ for skills** enables the skill picker after `$`.
- **Separate / commands** removes skills from the `/` picker.

Both default to disabled for DeepSeek: no skill catalog reaches the composer, so
skills remain available only through `/` when a catalog supplies them. The mock
harness used by tests follows the same preferences. Searches for a skill without
a catalog find nothing; enabling `$` alone makes a discovered skill available in
both pickers.

These controls change completion, not arbitrary typed text. Selecting a skill
stores its identity in the composer. Literal dollar signs, code, and unselected
text retain their meaning.

## Discovery and delivery

Discovery runs on the selected host in the project directory. The DeepSeek SDK
driver has no native command catalog of its own, and skills are not invented
either: Zeron reads the same files dsh would load (`<root>/<name>/SKILL.md`, or
a flat `<root>/<name>.md`, one level deep) from the project's `.dsh/skills`,
then `.agents/skills`, then the harness home's `skills`, the first root winning
a duplicate name (`crates/harness/src/deepseek/skills.rs`). A skill marked
`user-invocable: false` is listed but not offered, and a provider-advertised
slash command rides along when the driver has one. Mock (tests) reports the
same empty catalog.

Hosts advertise `composer-references-v1` for canonical reference delivery.
Selecting a harness command for an older host inserts literal slash text.
Sending an existing file, command, or skill chip to an unsupported host asks the
user to update that device and preserves the draft and attachments; the same
check applies when committing an edited queued message. Literal code examples
do not require this capability.

Provider references:

- [DeepSeek Harness SDK](https://github.com/deepseek-ai/deepseek-harness)

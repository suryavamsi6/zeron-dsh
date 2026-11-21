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
| `/files` | Open project files |
| `/terminal` | Open a terminal tab |
| `/rename` | Open the conversation rename dialog |
| `/stop` | Interrupt the current run |

The slash picker carries both the Zeron actions in the table above and the
DeepSeek driver's own two commands, `/goal` and `/compact`, advertised from
`commands()` in `crates/harness/src/deepseek/mod.rs`. This never changes what
the agent can do in its own terminal: only commands backed by a protocol or a
Zeron action are advertised.

Any legacy harness id still decodes so historical sessions render, but it has
no runtime, no command catalog, and cannot be selected for a new run.

## Skill preferences

Settings → Providers has two independent completion controls for the DeepSeek
agent on this device:

- **Use $ for skills** enables the skill picker after `$`.
- **Separate / commands** removes skills from the `/` picker.

Both default to on for DeepSeek: `$` opens the skill picker, and the skills a
chat discovers are kept out of `/`, so the two lists stay apart. Turning
"Separate / commands" off lists those skills beside the commands; turning
"Use $ for skills" off leaves `$` as ordinary text. The mock harness used by
tests follows the same controls. Skills come from the chat's catalog, so a
search without one finds nothing.

These controls change completion, not arbitrary typed text. Selecting a skill
stores its identity in the composer. Literal dollar signs, code, and unselected
text retain their meaning.

## Discovery and delivery

Discovery runs on the selected host in the project directory. Skills are
read, not invented: Zeron reads three roots, using the file shapes dsh loads
(`<root>/<name>/SKILL.md`, or a flat `<root>/<name>.md`, one level deep): the
project's `.dsh/skills`, then `.agents/skills`, then the harness home's
`skills`, the first root winning a duplicate name
(`crates/harness/src/deepseek/skills.rs`). A skill marked `user-invocable:
false` is listed but not offered. A discovered skill carries no slash command;
its `$` token is how it reaches a message. The DeepSeek SDK driver's own
commands are a separate catalog: `commands()` in the same module advertises
`/goal` and `/compact`. Mock (tests) reports none.

A pick stores the canonical reference link
(`apps/zgx/ui/screens/chat/invocations.luau`), and the engine decodes
invocation links without a capability check
(`crates/engine/src/sessions.rs`). The canonical link is the only shape, so
there is no host-specific fallback and nothing to negotiate. A capability
returns only together with the consumer that acts on it, when a second host
or engine exists.

Provider references:

- [DeepSeek Harness SDK](https://github.com/deepseek-ai/deepseek-harness)

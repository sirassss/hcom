# hcom setup and troubleshooting

Read this when the skill is invoked without arguments, when `hcom` is missing or
misbehaving, or when a message does not arrive.

## setup

1. run `hcom status` — if "command not found", install first:
   ```bash
   curl -fsSL https://github.com/aannoo/hcom/releases/latest/download/hcom-installer.sh | sh
   ```
   (or `brew install aannoo/hcom/hcom`, or `pip install hcom`)
2. run `hcom hooks add` to install hooks for all detected tools
3. restart the AI tool for hooks to activate

| status output | meaning | action |
|---------------|---------|--------|
| command not found | not installed | install, see above |
| `[~] claude` | tool exists, hooks not installed | `hcom hooks add` then restart |
| `[✓] claude` | hooks installed | ready |
| `[✗] claude` | tool not found | install the AI tool first |

Already installed as a plugin with `hcom` on `PATH`? Skip the installer — it is
for fresh hosts only.

## "hcom not working"

```bash
hcom status          # check installation
hcom hooks status    # check hooks specifically
hcom relay status    # check cross-device relay
```

hooks missing? `hcom hooks add` then restart tool.

still broken?
```bash
hcom reset all && hcom hooks add
# close all ai tool windows
hcom claude          # fresh start
```

## "messages not arriving"

| symptom | diagnosis | fix |
|---------|-----------|-----|
| agent not in `hcom list` | agent stopped or never bound | relaunch or wait for binding |
| agent shows `(not participating)` | never joined the bus | `hcom start --as <name>` inside the agent tab, then end the turn |
| message sent but not delivered | check `hcom events --last 5` | verify @mention matches agent name/tag |
| message reaches more than one agent | duplicate base name across tags | target the full `@tag-name` to hit exactly one |
| messages leaking between workflows | no thread isolation | script mode: always use `--thread`. interactive: use a tag instead |

## sandbox / permission issues

```bash
export HCOM_DIR="$PWD/.hcom"     # project-local mode
hcom hooks add                   # installs to project dir
```

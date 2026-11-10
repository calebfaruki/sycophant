# Examples

Values files for each provision command, rendered by `sycoadm <kind> up -f` with
[`charts/sycophant-commands`](../../charts/sycophant-commands/). Each names tenant
`hello-world`. Point one elsewhere with `--set <kind>.tenant=<name>`. Later `-f`
files override earlier ones, and `--set` overrides both.

The hello world is five pieces, each from one values file:

| Kind | Values |
|---|---|
| tenant | [`tenants/hello-world.yaml`](tenants/hello-world.yaml) |
| inference | [`inference/qwen3-5-2b.yaml`](inference/qwen3-5-2b.yaml) |
| toolset | [`toolsets/hello-world/values.yaml`](toolsets/hello-world/values.yaml), beside the image's `tools.yaml` |
| agent | [`agents/hello-world.yaml`](agents/hello-world.yaml) |
| channel | [`channels/tailnet.yaml`](channels/tailnet.yaml) |

[`users/alice.yaml`](users/alice.yaml) binds a person to the agent.
[`instructions/hello-world.yaml`](instructions/hello-world.yaml) delivers the
agent's instructions in [`instructions/hello-world/`](instructions/hello-world/)
through `sycoadm instructions up -f`.

The [README quickstart](../../README.md#quickstart) installs them in order.

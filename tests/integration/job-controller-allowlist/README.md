# job-controller-allowlist/

Only the chart-installed toolset controller ServiceAccount may create Jobs
with the `app.kubernetes.io/component=tool-job` label. Rule
`restrict-tool-job-labels` in the `cluster-protect-security` ClusterPolicy
enforces this.

Pair structure: the controller has an admit test (the canonical SA succeeds)
and an arbitrary-SA test (anyone else fails).

Belongs here: actor-by-name allowlists for resource creation.
Doesn't belong here: same-ns immutability rules (see `tenant-resource-protection/`).

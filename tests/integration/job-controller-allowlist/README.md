# job-controller-allowlist/

Only specific chart-installed controller ServiceAccounts may create Jobs
with the `app.kubernetes.io/component=llm-job|channel-job|airlock-job`
labels. Rules `restrict-hangar-job-labels` and `restrict-airlock-job-labels`
in the `cluster-protect-security` ClusterPolicy enforce this.

Pair structure: each controller has an admit test (the canonical SA succeeds)
and an arbitrary-SA test (anyone else fails).

Belongs here: actor-by-name allowlists for resource creation.
Doesn't belong here: same-ns immutability rules (see `tenant-resource-protection/`).

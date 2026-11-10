# harness-pod-shape/

Harness pods (labeled `app.kubernetes.io/part-of=sycophant` +
`component=harness`) must conform to a strict shape enforced by the
`cluster-gvisor-pod-policy`. `non-compliant-pods-rejected` has one step per CEL
rule. `compliant-pod-admits` is the happy-path admit test.

Tenant namespaces come from the tenant-namespace fixture, whose perimeter
label makes Kyverno generate the per-tenant policy binding before the test pod
submits. If the binding is missing, the policy cannot fire, and the test must
catch that as a red, not silently admit.

Belongs here: any rule `cluster-gvisor-pod-policy` enforces on harness and
capability-job pods. Doesn't belong here: Pod Security Admission rejections
(upstream, not sycophant) or non-pod resources.

# Copy this directory to start a new test:
#
#   cp -r tests/integration/_template tests/integration/<bucket>/<test-name>
#
# Then:
#   1. Replace <test-name> below with kebab-case name matching the directory.
#   2. Replace <description> with one plain-English sentence stating the
#      security claim this test verifies. This is the load-bearing field —
#      a test without a clear `description:` is a test no one can maintain.
#   3. Fill in the `steps:` block with try/finally/catch blocks.
#   4. If the test rejects a manifest, prefer `apply:` + `error:` with
#      `check:` blocks pinning the rejecter (PSA vs VAP vs Kyverno).
#      Use `script:` only for state-mutating setup (kubectl scale,
#      kubectl --as=) that can't be expressed declaratively.
#   5. Add the test to its bucket README.
apiVersion: chainsaw.kyverno.io/v1alpha1
kind: Test
metadata:
  name: <test-name>
spec:
  description: |
    <description: one sentence stating the security claim this test verifies>
  steps:
    - name: <step-name>
      try:
        - apply:
            file: ../../fixtures/<fixture>.yaml
            expect:
              - check:
                  ($error != null): true
                  (contains($stderr, '<expected-rejecter-text>')): true
      catch:
        - events: {}
      finally:
        - delete:
            ref:
              apiVersion: v1
              kind: <Kind>
              name: <name>
              namespace: <ns>
            expect:
              - check:
                  ($error != null): true

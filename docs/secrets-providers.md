# Provider Secrets: backend recipes

The sycophant chart consumes per-tenant LLM API keys via the Provider CRD's `secret: { name, key }` reference. The Tightbeam controller spawns ephemeral LLM Jobs that mount the referenced K8s Secret via projected volume; workspace pods never see API keys.

This doc shows minimal-working-example recipes for getting that Secret into the cluster. The chart imposes no preference among them — see [ADR 012](https://github.com/calebfaruki/sycophant) for the contract details.

## The contract

A Provider's `secret.name` must resolve to an **Opaque K8s Secret in the release namespace**, with `secret.key` (default `api-key`) containing the raw provider API token as bytes:

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: sycophant-llm-anthropic
  namespace: tenant-foo
type: Opaque
data:
  api-key: <base64-encoded-token>
```

How that Secret arrives is out of framework scope. The recipes below are equivalent from the chart's perspective.

## Recipe 1: kubectl create secret

Simplest. Right for small private deployments and local development.

```sh
kubectl create secret generic sycophant-llm-anthropic \
  --namespace tenant-foo \
  --from-literal=api-key="$ANTHROPIC_API_KEY"
```

Then in the chart's `values.yaml`:

```yaml
providers:
  anthropic:
    format: anthropic
    baseUrl: https://api.anthropic.com/v1
    secret:
      name: sycophant-llm-anthropic
      key: api-key
```

## Recipe 2: External Secrets Operator + AWS Secrets Manager

Right for cloud deployments where the operator already runs ESO and wants the API key managed alongside other cloud secrets.

Install ESO:

```sh
helm install external-secrets external-secrets/external-secrets -n external-secrets --create-namespace
```

Configure a `ClusterSecretStore` for AWS Secrets Manager (IRSA-bound SA, region of your AWS account):

```yaml
apiVersion: external-secrets.io/v1beta1
kind: ClusterSecretStore
metadata:
  name: aws-secrets-manager
spec:
  provider:
    aws:
      service: SecretsManager
      region: eu-central-1
      auth:
        jwt:
          serviceAccountRef:
            name: external-secrets-irsa
            namespace: external-secrets
```

Create an `ExternalSecret` in the tenant namespace pointing at a value stored at `prod/tenant-foo/anthropic`:

```yaml
apiVersion: external-secrets.io/v1beta1
kind: ExternalSecret
metadata:
  name: sycophant-llm-anthropic
  namespace: tenant-foo
spec:
  refreshInterval: 1h
  secretStoreRef:
    name: aws-secrets-manager
    kind: ClusterSecretStore
  target:
    name: sycophant-llm-anthropic
    creationPolicy: Owner
  data:
    - secretKey: api-key            # MUST match the Provider's secret.key (default "api-key")
      remoteRef:
        key: prod/tenant-foo/anthropic
```

**Key remapping gotcha**: ESO's `dataFrom` flows commonly produce secret keys matching the upstream JSON field name (`apiKey`, `ANTHROPIC_API_KEY`, etc.). The Provider's default `secret.key` is `api-key`. Use `data[].secretKey: api-key` (as above) to remap, or set the Provider's `secret.key` explicitly to whatever your remote produces.

Same chart values reference as Recipe 1.

Reference: <https://external-secrets.io/latest/api/externalsecret/>

## Recipe 3: Bitnami sealed-secrets

Right for GitOps workflows where encrypted secret manifests live in a public git repo, or for browser-direct customer-paste flows (a controller-encrypted manifest is committed; the cluster decrypts on apply).

Install the controller:

```sh
helm install sealed-secrets sealed-secrets/sealed-secrets -n infra --create-namespace
```

Encrypt a value with `kubeseal` against the controller's public cert:

```sh
echo -n "$ANTHROPIC_API_KEY" | kubeseal \
  --controller-namespace=infra \
  --raw \
  --namespace=tenant-foo \
  --name=sycophant-llm-anthropic
```

Wrap the resulting ciphertext in a SealedSecret manifest (or use `kubeseal -f secret.yaml -o yaml` to produce one from a regular Secret manifest):

```yaml
apiVersion: bitnami.com/v1alpha1
kind: SealedSecret
metadata:
  name: sycophant-llm-anthropic
  namespace: tenant-foo
spec:
  encryptedData:
    api-key: <ciphertext-from-kubeseal-output>
```

Apply the SealedSecret. The controller decrypts and creates the matching K8s Secret. Same chart values reference as Recipe 1.

**Scope note**: kubeseal's default `strict` scope binds the encrypted value to `namespace/name`. A SealedSecret encrypted for `tenant-foo/sycophant-llm-anthropic` cannot be unsealed in any other namespace or under any other name. Keep `strict` for tenant isolation; don't relax to `namespace-wide` or `cluster-wide` without a specific reason.

Reference: <https://github.com/bitnami-labs/sealed-secrets>

## Recipe 4: Vault Agent Injector (HashiCorp Vault or OpenBao)

Right for operators with an existing Vault deployment who want runtime sidecar-based secret delivery (secrets stay in pod memory, never materialize as K8s Secret objects).

Vault Agent Injector is a mutating admission webhook that adds a sidecar container to annotated pods. The sidecar authenticates to Vault, fetches secrets, and writes them to an in-memory volume. To make this satisfy the sycophant contract, the sidecar must write to a K8s Secret in the tenant namespace (Vault Agent has a `secret` output mode for this, or use Vault Secrets Operator instead).

Cleaner pattern for the sycophant contract: use **Vault Secrets Operator** (VSO) which syncs Vault KV values directly into K8s Secrets. Install per upstream docs, then create a `VaultStaticSecret` resource:

```yaml
apiVersion: secrets.hashicorp.com/v1beta1
kind: VaultStaticSecret
metadata:
  name: sycophant-llm-anthropic
  namespace: tenant-foo
spec:
  vaultAuthRef: default
  mount: secret
  type: kv-v2
  path: tenants/tenant-foo/anthropic
  destination:
    create: true
    name: sycophant-llm-anthropic
```

Same chart values reference as Recipe 1.

For OpenBao users: openbao-k8s is an MPL-2.0 fork of vault-k8s with identical APIs. Substitute `openbao.org/agent-inject` for `vault.hashicorp.com/agent-inject` annotations.

References:
- Vault Secrets Operator: <https://developer.hashicorp.com/vault/docs/platform/k8s/vso>
- OpenBao k8s: <https://github.com/openbao/openbao-k8s>

## What the chart does not do

- The chart does not install ESO, sealed-secrets, Vault, Vault Agent Injector, OpenBao, or any other secrets backend.
- The chart does not assume which backend produced the K8s Secret it consumes.
- The chart does not advise. Picking a backend is the deployer's decision based on their existing infrastructure, compliance posture, and operational preference.

## Migrating between backends

Because the chart consumes only the resulting K8s Secret, migrating from one backend to another (e.g., `kubectl` → sealed-secrets, or sealed-secrets → ESO+Vault) is a deployer-side operation. The Provider CRD reference doesn't change. The chart doesn't need a release event. Migrate one tenant at a time if you want; the chart doesn't know the difference.

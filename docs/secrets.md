# Secrets: backend recipes

The sycophant chart consumes per-tenant LLM API keys via a toolset profile's `secrets` list. The toolset controller spawns ephemeral Jobs that mount the referenced K8s Secret by reference; harness pods never see API keys, and the controller never reads one.

This doc shows minimal-working-example recipes for getting that Secret into the cluster. The chart imposes no preference among them — choose by ops cost vs. blast radius vs. existing tooling in your cluster.

## The contract

A profile's `secrets[].secret` must resolve to an **Opaque K8s Secret in the release namespace**, holding the raw provider API token as bytes under **a data key equal to the Secret's own name**:

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: sycophant-llm-openrouter
  namespace: tenant-foo
type: Opaque
data:
  sycophant-llm-openrouter: <base64-encoded-token>
```

The data key is not configurable. `syco tenant secret set <name>` writes this shape; hand-rolled Secrets must match it.

Each entry sets exactly one of `env` or `file`, which decides how the value reaches the worker:

```yaml
secrets:
  - secret: sycophant-llm-openrouter
    env: TOOLSET_API_KEY        # secretKeyRef environment variable
  - secret: sycophant-ssh-key
    file: /run/secrets/toolset/id_ed25519   # read-only Secret-backed volume
```

How that Secret arrives is out of framework scope. The recipes below are equivalent from the chart's perspective.

## Recipe 1: kubectl create secret

Simplest. Right for small private deployments and local development.

```sh
printf '%s' "$OPENROUTER_API_KEY" | syco tenant secret set sycophant-llm-openrouter --ns tenant-foo
```

The `kubectl` equivalent, if you are not using the CLI — note the data key repeats the Secret name:

```sh
kubectl create secret generic sycophant-llm-openrouter \
  --namespace tenant-foo \
  --from-literal=sycophant-llm-openrouter="$OPENROUTER_API_KEY"
```

Then in the chart's `values.yaml`:

```yaml
toolsets:
  prompt:
    image: prompt-toolset
    profiles:
      default:
        secrets:
          - secret: sycophant-llm-openrouter
            file: /run/secrets/toolset/api-key
        egress:
          - { domain: openrouter.ai, port: 443 }
        TOOLSET_FORMAT: openai
        TOOLSET_MODEL: deepseek/deepseek-v4-flash
        TOOLSET_BASE_URL: https://openrouter.ai/api/v1
```

`secrets` and `egress` are governed keys. Every other key in a profile is inert and forwards verbatim to the worker as an environment variable.

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

Create an `ExternalSecret` in the tenant namespace pointing at a value stored at `prod/tenant-foo/openrouter`:

```yaml
apiVersion: external-secrets.io/v1beta1
kind: ExternalSecret
metadata:
  name: sycophant-llm-openrouter
  namespace: tenant-foo
spec:
  refreshInterval: 1h
  secretStoreRef:
    name: aws-secrets-manager
    kind: ClusterSecretStore
  target:
    name: sycophant-llm-openrouter
    creationPolicy: Owner
  data:
    - secretKey: sycophant-llm-openrouter   # MUST equal target.name
      remoteRef:
        key: prod/tenant-foo/openrouter
```

**Key remapping gotcha**: ESO's `dataFrom` flows commonly produce secret keys matching the upstream JSON field name (`apiKey`, `OPENROUTER_API_KEY`, etc.). The consuming data key is not configurable, so remapping is mandatory: set `data[].secretKey` to the Secret name (as above). A `dataFrom` flow that copies the remote key through verbatim will not resolve.

Same chart values reference as Recipe 1.

Reference: <https://external-secrets.io/latest/api/externalsecret/>

## Recipe 3: Bitnami sealed-secrets

Right for GitOps workflows where encrypted secret manifests live in a public git repo, or for browser-direct customer-paste flows (a controller-encrypted manifest is committed; the cluster decrypts on apply).

Install the controller:

```sh
helm install sealed-secrets sealed-secrets/sealed-secrets -n sycophant-system --create-namespace
```

Encrypt a value with `kubeseal` against the controller's public cert:

```sh
echo -n "$OPENROUTER_API_KEY" | kubeseal \
  --controller-namespace=infra \
  --raw \
  --namespace=tenant-foo \
  --name=sycophant-llm-openrouter
```

Wrap the resulting ciphertext in a SealedSecret manifest (or use `kubeseal -f secret.yaml -o yaml` to produce one from a regular Secret manifest):

```yaml
apiVersion: bitnami.com/v1alpha1
kind: SealedSecret
metadata:
  name: sycophant-llm-openrouter
  namespace: tenant-foo
spec:
  encryptedData:
    sycophant-llm-openrouter: <ciphertext-from-kubeseal-output>
```

Apply the SealedSecret. The controller decrypts and creates the matching K8s Secret. Same chart values reference as Recipe 1.

**Scope note**: kubeseal's default `strict` scope binds the encrypted value to `namespace/name`. A SealedSecret encrypted for `tenant-foo/sycophant-llm-openrouter` cannot be unsealed in any other namespace or under any other name. Keep `strict` for tenant isolation; don't relax to `namespace-wide` or `cluster-wide` without a specific reason.

Reference: <https://github.com/bitnami-labs/sealed-secrets>

## Recipe 4: Vault Agent Injector (HashiCorp Vault or OpenBao)

Right for operators with an existing Vault deployment who want runtime sidecar-based secret delivery (secrets stay in pod memory, never materialize as K8s Secret objects).

Vault Agent Injector is a mutating admission webhook that adds a sidecar container to annotated pods. The sidecar authenticates to Vault, fetches secrets, and writes them to an in-memory volume. To make this satisfy the sycophant contract, the sidecar must write to a K8s Secret in the tenant namespace (Vault Agent has a `secret` output mode for this, or use Vault Secrets Operator instead).

Cleaner pattern for the sycophant contract: use **Vault Secrets Operator** (VSO) which syncs Vault KV values directly into K8s Secrets. Install per upstream docs, then create a `VaultStaticSecret` resource:

```yaml
apiVersion: secrets.hashicorp.com/v1beta1
kind: VaultStaticSecret
metadata:
  name: sycophant-llm-openrouter
  namespace: tenant-foo
spec:
  vaultAuthRef: default
  mount: secret
  type: kv-v2
  path: tenants/tenant-foo/openrouter
  destination:
    create: true
    name: sycophant-llm-openrouter
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

Because the chart consumes only the resulting K8s Secret, migrating from one backend to another (e.g., `kubectl` → sealed-secrets, or sealed-secrets → ESO+Vault) is a deployer-side operation. The profile's `secrets` reference doesn't change. The chart doesn't need a release event. Migrate one tenant at a time if you want; the chart doesn't know the difference.

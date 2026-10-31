# shellcheck shell=bash
# Store-side access to the instruction bucket, through the filer inside the
# store pod.
readonly FILER="http://localhost:8888/buckets/sycophant-instructions"

folder() { printf 'system:serviceaccount:%s:ws-a' "$1"; }

# store_put <tenant-ns> <content>: AGENTS.md in that tenant's ws-a folder.
store_put() {
  printf '%s' "$2" | kubectl exec -i -n sycophant-system deploy/seaweedfs -- \
    curl -fsS -g -o /dev/null -T - "${FILER}/$(folder "$1")/AGENTS.md"
}

store_rm() {
  kubectl exec -n sycophant-system deploy/seaweedfs -- \
    curl -fsS -g -o /dev/null -X DELETE "${FILER}/$(folder "$1")/?recursive=true"
}

# store_status <tenant-ns>: the HTTP status of that tenant's AGENTS.md.
store_status() {
  kubectl exec -n sycophant-system deploy/seaweedfs -- \
    curl -sS -g -o /dev/null -w '%{http_code}' "${FILER}/$(folder "$1")/AGENTS.md"
}

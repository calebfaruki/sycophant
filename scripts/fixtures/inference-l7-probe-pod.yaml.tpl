apiVersion: v1
kind: Pod
metadata:
  name: inference-l7-probe
  labels:
    app.kubernetes.io/part-of: sycophant
    app.kubernetes.io/component: harness
    sycophant.md/agent: hello-world
spec:
  restartPolicy: Never
  automountServiceAccountToken: false
  securityContext:
    runAsNonRoot: true
    runAsUser: 65534
    seccompProfile: { type: RuntimeDefault }
  containers:
    - name: probe
      image: busybox:1.36
      resources:
        requests: { cpu: 50m, memory: 64Mi }
        limits: { cpu: 50m, memory: 64Mi }
      securityContext:
        allowPrivilegeEscalation: false
        readOnlyRootFilesystem: true
        runAsNonRoot: true
        capabilities: { drop: ["ALL"] }
      command: ["sh", "-c"]
      args:
        - |
          b=http://${svc}:${INFERENCE_PORT}
          r=$(wget -S -q -O- --header 'Content-Type: application/json' --post-data '{"model":"${INFERENCE_PROFILE}","messages":[{"role":"user","content":"Reply with a short greeting."}],"stream":false,"max_tokens":32}' "$b/v1/chat/completions" 2>&1 || true)
          echo "COMPLETION=$(printf '%s' "$r" | grep -o 'HTTP/[0-9.]* [0-9][0-9][0-9]' | tail -1 | grep -o '[0-9][0-9][0-9]$')"
          printf '%s' "$r" | tr ',{}' '\n' | grep -oE '"(completion_tokens|prompt_ms|predicted_ms|predicted_per_second)":[0-9.]+'
          t=$(wget -S -q -O- --header 'Content-Type: application/json' --post-data '{"model":"${INFERENCE_PROFILE}","messages":[{"role":"user","content":"What is the weather in Paris right now? You must call the get_weather tool to answer."}],"tools":[{"type":"function","function":{"name":"get_weather","description":"Get the current weather for a city.","parameters":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}}}],"tool_choice":"auto","stream":false}' "$b/v1/chat/completions" 2>&1 || true)
          echo "TOOLCALL=$(printf '%s' "$t" | grep -o 'HTTP/[0-9.]* [0-9][0-9][0-9]' | tail -1 | grep -o '[0-9][0-9][0-9]$')"
          if printf '%s' "$t" | grep -qE '"tool_calls":[[:space:]]*\[[[:space:]]*\{'; then echo "TOOLCALLS=present"; else echo "TOOLCALLS=absent"; fi

# Source of docs/demo.gif. Record with VHS (https://github.com/charmbracelet/vhs):
#
#   git clone --depth 500 https://github.com/kubernetes/kubernetes /tmp/k8s
#   (cd /tmp/k8s && wn init)          # pre-index once: several minutes (~20k indexed files)
#   WN_DEMO_REPO=/tmp/k8s vhs docs/demo.tape
#
# The recording used kubernetes HEAD 7c596ad7938 (2026-09-30, clone depth 500), gemma-xl1 and a warm daemon.
# The three queries are the titles (two lightly shortened) of real bug reports: #141298, #142526 and #141488. Their fixes
# changed pkg/controller/podautoscaler/replica_calculator.go,
# staging/src/k8s.io/apimachinery/pkg/util/yaml/decoder.go and pkg/proxy/winkernel/hns.go, and wn
# put each of those at #1. Scores and timings vary by machine and kubernetes revision.

Output docs/demo.gif
Set Shell "bash"
Set FontSize 20
Set Width 1280
Set Height 680
Set Padding 24
Set TypingSpeed 28ms
Set Theme "Catppuccin Mocha"

Hide
Type "export PS1='$ ' && cd \"$WN_DEMO_REPO\" && wn ask 'warm up the daemon' >/dev/null 2>&1; clear" Enter
Sleep 8s
Show

Type "# Kubernetes, ~20k indexed files. Real bug report #141298: 'Succeeded pods cause incorrect HPA calculation'" Enter
Sleep 1.5s
Type "rg -li hpa | wc -l" Enter
Sleep 2s
Type "time wn ask 'Succeeded pods cause incorrect HPA calculation'" Enter
Sleep 3.5s

Type "# Another (#142526), nothing specific to grep for:" Enter
Type "wn ask 'yaml: last line silently dropped when its length is a multiple of the buffer size'" Enter
Sleep 3.5s

Type "# Error text works too (#141488):" Enter
Type "wn ask 'kube-proxy panics with index out of range when an HNS load balancer has no ports'" Enter
Sleep 3.5s

Type "# Know the exact symbol? rg is still the right tool:" Enter
Type "rg -l PodsByCreationTime" Enter
Sleep 3s

Type "# The real fixes changed replica_calculator.go, decoder.go and hns.go." Enter
Sleep 3s

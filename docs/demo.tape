# Source of docs/demo.gif. Record with VHS (https://github.com/charmbracelet/vhs):
#
#   git clone --depth 500 https://github.com/kubernetes/kubernetes /tmp/k8s
#   (cd /tmp/k8s && wn init)          # pre-index once: several minutes (~20k indexed files)
#   WN_DEMO_REPO=/tmp/k8s vhs docs/demo.tape
#
# The recording used kubernetes HEAD 7c596ad7938 (wn v0.1.0 release binary) (2026-09-30, clone depth 500), gemma-xl1 and a warm daemon.
# The three queries are the titles (two lightly shortened) of real bug reports: #141298, #142526 and #141488. Their fixes
# changed pkg/controller/podautoscaler/replica_calculator.go,
# staging/src/k8s.io/apimachinery/pkg/util/yaml/decoder.go and pkg/proxy/winkernel/hns.go, and wn
# put each of those at #1. Scores and timings vary by machine and kubernetes revision.

Output docs/demo.gif
Set Shell "bash"
Set FontSize 20
Set Width 1200
Set Height 340
Set Padding 28
Set TypingSpeed 35ms
Set Theme "Catppuccin Mocha"

Hide
Type `export PS1='$ ' && cd "$WN_DEMO_REPO" && wn ask 'warm up the daemon' >/dev/null 2>&1; clear` Enter
Sleep 8s
Show

Type "# Kubernetes (~20k files). Bug report #141298:" Enter
Type "# 'Succeeded pods cause incorrect HPA calculation'" Enter
Sleep 2s
Type "rg -li hpa | wc -l"
Sleep 800ms
Enter
Sleep 2.5s
Type "wn ask 'Succeeded pods cause incorrect HPA calculation'"
Sleep 800ms
Enter
Sleep 5s
Hide
Type "clear" Enter
Show

Type "# Bug #142526, nothing specific to grep for:" Enter
Sleep 1s
Type "wn ask 'yaml: last line dropped when its length is a multiple of the buffer size'"
Sleep 800ms
Enter
Sleep 5s
Hide
Type "clear" Enter
Show

Type "# Bug #141488, from the panic message:" Enter
Sleep 1s
Type "wn ask 'kube-proxy panics with index out of range when an HNS load balancer has no ports'"
Sleep 800ms
Enter
Sleep 5s
Hide
Type "clear" Enter
Show

Type "# Know the exact symbol? rg is still the right tool:" Enter
Sleep 1s
Type "rg -l PodsByCreationTime"
Sleep 800ms
Enter
Sleep 3s
Type "# The real fixes changed replica_calculator.go, decoder.go and hns.go." Enter
Sleep 4s

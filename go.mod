module github.com/quay/clair-operator

go 1.16

require (
	github.com/go-logr/logr v0.3.0
	github.com/go-logr/zapr v0.2.0
	github.com/google/go-cmp v0.5.5 // indirect
	github.com/onsi/ginkgo v1.14.1
	github.com/onsi/gomega v1.10.2
	github.com/openshift/api v3.9.0+incompatible
	github.com/prometheus-operator/prometheus-operator/pkg/apis/monitoring v0.45.0
	github.com/quay/clair/v4 v4.0.0
	go.uber.org/zap v1.15.0
	golang.org/x/oauth2 v0.0.0-20210413134643-5e61552d6c78 // indirect
	gomodules.xyz/jsonpatch/v2 v2.1.0
	gopkg.in/yaml.v3 v3.0.0-20200615113413-eeeca48fe776
	k8s.io/api v0.20.2
	k8s.io/apimachinery v0.20.2
	k8s.io/client-go v0.20.2
	sigs.k8s.io/controller-runtime v0.8.3
	sigs.k8s.io/controller-tools v0.4.1
	sigs.k8s.io/kustomize/api v0.8.6
	sigs.k8s.io/kustomize/kustomize/v3 v3.10.0
)

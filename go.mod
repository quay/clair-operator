module github.com/quay/clair-operator

go 1.15

require (
	github.com/Azure/go-autorest v14.2.0+incompatible // indirect
	github.com/go-logr/logr v0.3.0
	github.com/onsi/ginkgo v1.14.1
	github.com/onsi/gomega v1.10.2
	github.com/openshift/api v3.9.0+incompatible
	github.com/prometheus-operator/prometheus-operator/pkg/apis/monitoring v0.45.0
	github.com/quay/clair/v4 v4.0.0
	gomodules.xyz/jsonpatch/v2 v2.1.0
	gopkg.in/yaml.v3 v3.0.0-20200615113413-eeeca48fe776
	k8s.io/api v0.19.2
	k8s.io/apimachinery v0.19.2
	k8s.io/client-go v0.19.2
	sigs.k8s.io/controller-runtime v0.7.0
)

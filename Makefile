# Current Operator version
VERSION ?= 0.0.1
# Default bundle image tag
BUNDLE_IMG ?= controller-bundle:$(VERSION)
# Options for 'bundle-build'
ifneq ($(origin CHANNELS), undefined)
BUNDLE_CHANNELS := --channels=$(CHANNELS)
endif
ifneq ($(origin DEFAULT_CHANNEL), undefined)
BUNDLE_DEFAULT_CHANNEL := --default-channel=$(DEFAULT_CHANNEL)
endif
BUNDLE_METADATA_OPTS ?= $(BUNDLE_CHANNELS) $(BUNDLE_DEFAULT_CHANNEL)

# Image URL to use all building/pushing image targets
IMG ?= controller:latest

# Get the currently used golang install path (in GOPATH/bin, unless GOBIN is set)
ifeq (,$(shell go env GOBIN))
GOBIN=$(shell go env GOPATH)/bin
else
GOBIN=$(shell go env GOBIN)
endif

all: manager

# Run tests
ENVTEST_ASSETS_DIR=$(shell pwd)/testbin
.PHONY: test
test: generate
	go fmt ./...
	go vet ./...
	source ${ENVTEST_ASSETS_DIR}/setup-envtest.sh;\
		fetch_envtest_tools $(ENVTEST_ASSETS_DIR);\
		setup_envtest_env $(ENVTEST_ASSETS_DIR);\
		go test ./... -coverprofile cover.out

testbin/setup-envtest.sh: go.mod
	curl -sSLo $@ https://raw.githubusercontent.com/kubernetes-sigs/controller-runtime/$$(go list -m sigs.k8s.io/controller-runtime | awk '{print $$2}')/hack/setup-envtest.sh

# Build manager binary
manager: generate
	go build -o bin/manager main.go

# Run against the configured Kubernetes cluster in ~/.kube/config
run: generate
	go run ./main.go

# Install CRDs into a cluster
install: manifests bin/kustomize
	$(KUSTOMIZE) build config/crd | kubectl apply -f -

# Uninstall CRDs from a cluster
uninstall: manifests bin/kustomize
	$(KUSTOMIZE) build config/crd | kubectl delete -f -

# Deploy controller in the configured Kubernetes cluster in ~/.kube/config
deploy: manifests bin/kustomize
	cd config/manager && $(KUSTOMIZE) edit set image controller=${IMG}
	$(KUSTOMIZE) build config/default | kubectl apply -f -

# UnDeploy controller from the configured Kubernetes cluster in ~/.kube/config
undeploy:
	$(KUSTOMIZE) build config/default | kubectl delete -f -

# Generate code and manifests e.g. CRD, RBAC etc.
.PHONY: generate
generate:
	go generate -x ./...

# Build the docker image
docker-build: test
	docker build -t ${IMG} .

# Push the docker image
docker-push:
	docker push ${IMG}

# Download controller-gen locally if necessary
CONTROLLER_GEN = $(shell pwd)/bin/controller-gen
controller-gen: bin/controller-gen
bin/controller-gen: go.mod
	GOBIN=$(shell git rev-parse --show-toplevel)/bin go install sigs.k8s.io/controller-tools/cmd/controller-gen

# Download kustomize locally if necessary
KUSTOMIZE = $(shell pwd)/bin/kustomize
bin/kustomize: go.mod
	GOBIN=$(shell git rev-parse --show-toplevel)/bin go install sigs.k8s.io/kustomize/kustomize/v4

# Generate bundle manifests and metadata, then validate generated files.
.PHONY: bundle
bundle: bin/kustomize
	operator-sdk generate kustomize manifests -q
	cd config/manager && $(KUSTOMIZE) edit set image controller=$(IMG)
	$(KUSTOMIZE) build config/manifests | operator-sdk generate bundle -q --overwrite --version $(VERSION) $(BUNDLE_METADATA_OPTS)
	operator-sdk bundle validate ./bundle

# Build the bundle image.
.PHONY: bundle-build
bundle-build:
	docker build -f bundle.Dockerfile -t $(BUNDLE_IMG) .

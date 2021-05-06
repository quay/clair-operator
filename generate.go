package main

//go:generate make --silent bin/controller-gen
//go:generate bin/controller-gen object:headerFile="hack/boilerplate.go.txt" paths="./..."
//go:generate bin/controller-gen crd:trivialVersions=true,preserveUnknownFields=false rbac:roleName=manager-role webhook paths="./..." output:crd:artifacts:config=config/crd/bases

DESTDIR := src
VERSION := 1.3.0
V := v$(word 1,$(subst ., ,$(VERSION)))
types := gatewayclasses gateways grpcroutes httproutes referencegrants
SRCs = $(foreach t,$(types),$(DESTDIR)/$V/$t.rs)

.PHONY: all
all: $(SRCs)

$(DESTDIR)/$V:
	mkdir -p $@

$(DESTDIR)/$V/%.rs: Makefile $(DESTDIR)/$V
	curl -sSfL 'https://github.com/kubernetes-sigs/gateway-api/raw/refs/tags/v$(VERSION)/config/crd/standard/gateway.networking.k8s.io_$*.yaml' |\
		kopium --auto --derive Default --smart-derive-elision --filename - \
		>$@

package v1alpha1

import (
	"context"
	"errors"
	"fmt"
	"net/http"

	"github.com/go-logr/logr"
	"github.com/quay/clair/v4/config"
	"gopkg.in/yaml.v3"
	"sigs.k8s.io/controller-runtime/pkg/webhook/admission"
)

var (
	errMissingLabel = errors.New("missing required label (how?)")
	errBadKind      = errors.New("request object is neither Secret nor ConfigMap")
)

// +kubebuilder:webhook:path=/validate-clair-config,mutating=false,sideEffects=none,failurePolicy=fail,groups="",resources=configmaps;secrets,verbs=create;update,versions=v1,name=vconfig.c.pq.io,admissionReviewVersions=v1;v1beta1

// ConfigValidator is a validating webhook that disallows updates or creations
// of labelled ConfigMaps or Secrets with malformed Clair configurations.
//
// +kubebuilder:object:generate=false
type ConfigValidator struct {
	configCommon
}

// Handle implements admission.Handler.
func (v *ConfigValidator) Handle(ctx context.Context, req admission.Request) admission.Response {
	log := configlog.
		WithName("validator").
		WithValues("uid", req.UID)
	ctx = logr.NewContext(ctx, log)
	log.Info("examining object",
		"namespace", req.Namespace,
		"name", req.Name,
		"kind", req.Kind)

	// Populate the labels, annotations, and data.
	// Errors reported here are errors in the request.
	d, err := v.details(ctx, req)
	if err != nil {
		return admission.Errored(http.StatusBadRequest, err)
	}
	// Grab the config version from the label. If the label doesn't exist, how
	// did this validator even get called?
	version, ok := d.labels[ConfigLabel]
	if !ok {
		return admission.Errored(http.StatusBadRequest, errMissingLabel)
	}
	log.V(2).Info("labelled as", "version", version)

	// Find the key containing the configuration blob.
	key, ok := d.annotations[ConfigKey]
	if !ok {
		return admission.Denied("missing required annotation: indicate key containing config")
	}
	log.V(2).Info("config at", "key", key)

	// Grab the blob.
	cfg, ok := d.item(key)
	if !ok {
		return admission.Denied(fmt.Sprintf("missing value: indicated key %q does not exist", key))
	}

	if err := validateConfig(ctx, version, cfg); err != nil {
		return admission.Denied(fmt.Sprintf("config validation failed: %v", err))
	}
	return admission.Allowed("")
}

// ValidateConfig is the workhorse function that takes raw bytes and is
// responsible for checking correctness. A nil error is reported if the config
// is valid.
//
// A version string is passed for forwards compatibility.
func validateConfig(ctx context.Context, v string, b []byte) error {
	if err := ctx.Err(); err != nil {
		return err
	}

	switch v {
	case ConfigLabelV1:
		var c config.Config
		if err := yaml.Unmarshal(b, &c); err != nil {
			return err
		}
		for _, m := range []string{"indexer", "matcher", "notifier"} {
			c.Mode = m
			if err := config.Validate(c); err != nil {
				return err
			}
		}
	default:
		return fmt.Errorf("unknown config version: %q", v)
	}

	// Additional Validation?
	return nil
}

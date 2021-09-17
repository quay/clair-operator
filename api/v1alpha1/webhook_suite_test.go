/*
Copyright 2021 The Clair authors.

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

    http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.
*/

package v1alpha1

import (
	"bytes"
	"context"
	"crypto/tls"
	_ "embed"
	"io"
	"io/ioutil"
	"net"
	"path"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/go-logr/zapr"
	"go.uber.org/zap/zapcore"
	"go.uber.org/zap/zaptest"
	admissionv1beta1 "k8s.io/api/admission/v1beta1"
	corev1 "k8s.io/api/core/v1"
	"k8s.io/apimachinery/pkg/runtime"
	ctrl "sigs.k8s.io/controller-runtime"
	"sigs.k8s.io/controller-runtime/pkg/client"
	"sigs.k8s.io/controller-runtime/pkg/envtest"
	"sigs.k8s.io/controller-runtime/pkg/log"
	"sigs.k8s.io/kustomize/api/filesys"
	"sigs.k8s.io/kustomize/api/krusty"
	// +kubebuilder:scaffold:imports
)

func TestWebhooks(t *testing.T) {
	ctx, c := webhookSetup(context.Background(), t)
	t.Log("webhooks set up")
	t.Run("Validating", testValidating(ctx, c)) // see validating_webhook_test.go
	t.Run("Mutating", testMutating(ctx, c))     // see mutating_webhook_test.go
}

func webhookSetup(ctx context.Context, t testing.TB) (context.Context, client.Client) {
	logger := zapr.NewLogger(zaptest.NewLogger(t, zaptest.Level(zapcore.DebugLevel)))
	ctx = log.IntoContext(ctx, logger)

	ctx, done := context.WithCancel(ctx)
	t.Cleanup(done)

	kopts := krusty.MakeDefaultOptions()
	res, err := krusty.MakeKustomizer(kopts).Run(
		filesys.MakeFsOnDisk(),
		filepath.Join("..", "..", "config", "webhook"))
	if err != nil {
		t.Fatal(err)
	}
	resDir := t.TempDir()
	for _, r := range res.Resources() {
		f, err := ioutil.TempFile(resDir, "*.json")
		if err != nil {
			t.Fatal(err)
		}
		defer f.Close()
		b, err := r.MarshalJSON()
		if err != nil {
			t.Fatal(err)
		}
		if _, err = io.Copy(f, bytes.NewReader(b)); err != nil {
			t.Fatal(err)
		}
	}

	env := envtest.Environment{
		CRDDirectoryPaths: []string{filepath.Join("..", "..", "config", "crd", "bases")},
		WebhookInstallOptions: envtest.WebhookInstallOptions{
			Paths: []string{resDir},
		},
	}
	cfg, err := env.Start()
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := env.Stop(); err != nil {
			t.Log(err)
		}
	})

	scheme := runtime.NewScheme()
	for _, f := range []func(*runtime.Scheme) error{
		AddToScheme,
		corev1.AddToScheme,
		admissionv1beta1.AddToScheme,
	} {
		if err := f(scheme); err != nil {
			t.Fatal(err)
		}
	}

	// +kubebuilder:scaffold:scheme

	cl, err := client.New(cfg, client.Options{Scheme: scheme})
	if err != nil {
		t.Fatal(err)
	}

	// start webhook server using Manager
	mgr, err := ctrl.NewManager(cfg, ctrl.Options{
		Scheme:             scheme,
		Host:               env.WebhookInstallOptions.LocalServingHost,
		Port:               env.WebhookInstallOptions.LocalServingPort,
		CertDir:            env.WebhookInstallOptions.LocalServingCertDir,
		LeaderElection:     false,
		MetricsBindAddress: "0",
		Logger:             logger,
	})
	if err != nil {
		t.Fatal(err)
	}

	if err := SetupConfigWebhooks(mgr); err != nil {
		t.Fatal(err)
	}

	// +kubebuilder:scaffold:webhook

	go func() {
		ctx, cancel := context.WithCancel(ctx)
		t.Cleanup(cancel)
		if err = mgr.Start(ctx); err != nil {
			t.Error(err)
		}
	}()
	// wait for the webhook server to get ready
	dialer := &net.Dialer{Timeout: time.Second}
	addrPort := net.JoinHostPort(
		env.WebhookInstallOptions.LocalServingHost,
		strconv.Itoa(env.WebhookInstallOptions.LocalServingPort))
	tctx, done := context.WithTimeout(ctx, time.Minute)
	defer done()
Wait:
	for {
		select {
		case <-tctx.Done():
			t.Fatal(tctx.Err())
		default:
			conn, err := tls.DialWithDialer(dialer, "tcp", addrPort, &tls.Config{InsecureSkipVerify: true})
			if err == nil {
				conn.Close()
				break Wait
			}
			t.Log(err)
		}
	}

	return ctx, cl
}

type webhookTestcase struct {
	Name  string
	Setup func(testing.TB, ConfigObject)
	Check func(testing.TB, ConfigObject, error)
	Err   bool
}

func CheckErr(t testing.TB, _ ConfigObject, err error) {
	if err == nil {
		t.Error("expected error")
	}
}

func (tc webhookTestcase) Run(ctx context.Context, c client.Client, in client.Object) func(t *testing.T) {
	var o ConfigObject
	switch t := in.(type) {
	case *corev1.Secret:
		o = &secretConfig{t}
	case *corev1.ConfigMap:
		o = &configmapConfig{t}
	default:
		panic("programmer error")
	}
	return func(t *testing.T) {
		tc.Setup(t, o)
		err := c.Create(ctx, in)
		if err != nil {
			t.Log(err)
		} else {
			t.Logf("created: %s", in.GetName())
			t.Cleanup(func() {
				if err := c.Delete(ctx, in); err != nil {
					t.Log(err)
				}
			})
		}
		if tc.Check != nil {
			tc.Check(t, o, err)
		} else {
			if err != nil {
				t.Error(err)
			}
		}
	}
}

func mkConfigMap(t testing.TB) *corev1.ConfigMap {
	c := &corev1.ConfigMap{}
	c.Namespace = "default"
	c.GenerateName = strings.ToLower(path.Base(t.Name())) + `-`
	c.Data = make(map[string]string)
	c.Labels = make(map[string]string)
	c.Annotations = make(map[string]string)
	return c
}

func mkSecret(t testing.TB) *corev1.Secret {
	c := &corev1.Secret{}
	c.Namespace = "default"
	c.GenerateName = strings.ToLower(path.Base(t.Name())) + `-`
	c.StringData = make(map[string]string)
	c.Labels = make(map[string]string)
	c.Annotations = make(map[string]string)
	return c
}

type ConfigObject interface {
	GetLabels() map[string]string
	SetLabels(map[string]string)
	GetAnnotations() map[string]string
	SetAnnotations(map[string]string)
	GetItem(key string) string
	SetItem(key, val string)
}

type (
	configmapConfig struct{ *corev1.ConfigMap }
	secretConfig    struct{ *corev1.Secret }
)

var (
	_ ConfigObject = (*configmapConfig)(nil)
	_ ConfigObject = (*secretConfig)(nil)
)

func (c *configmapConfig) GetItem(key string) string {
	v, ok := c.Data[key]
	if ok {
		return v
	}
	b, ok := c.BinaryData[key]
	if !ok {
		return ""
	}
	return string(b)
}

func (c *configmapConfig) SetItem(key, val string) {
	c.Data[key] = val
}

func (s *secretConfig) GetItem(key string) string {
	return string(s.Data[key])
}

func (s *secretConfig) SetItem(key, val string) {
	s.StringData[key] = val
}

var (
	//go:embed testdata/notyaml
	invalidYAML string
	//go:embed testdata/invalid.yaml
	invalidConfig string
	//go:embed testdata/simple.yaml
	simpleConfig string
	//go:embed testdata/missing_service.yaml
	missingServiceConfig string
	//go:embed testdata/with_secret.yaml
	secretRefConfig string
	//go:embed testdata/with_secret.rendered.yaml
	secretRefConfigRendered string
	//go:embed testdata/templating.yaml
	allRefConfig string
	//go:embed testdata/templating.rendered.yaml
	allRefConfigRendered string
	//go:embed testdata/bad_templating.yaml
	allRefIncorrect string
)

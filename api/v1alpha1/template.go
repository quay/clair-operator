/*
Templating

The MutatingWebhook ([ConfigMutator]) in this package implements a URI-based
templating language that allows values to be looked up and replaced with values
from the current state of the kubernetes cluster. See the documentation on that
type for how to configure the behaviour of the templator.

The following URI schemes are supported to be replaced in templated configs:

secret

These URIs must not have trailing slashes, and require a URL parameter "key".
If there are multiple keys provided, they will be joined with the supplied
parameter "join". Given a secret "my-secret" with data "username=user" and
"password=pass", "secret:my-secret?key=password" would become "pass" and
"secret:my-secret?key=username,password&join=%3A" would become "user:pass".
These URIs can only be used from an input that came from a Secret object.

configmap

The "configmap" scheme behaves the same as the "secret" scheme, except that
it references ConfigMap objects instead of Secret objects, and does not have
the input restriction -- that is, it can be used with inputs from both
Secrets and ConfigMaps.

database+<kind>

This scheme interprets the opaque portion as a database connection string for
"kind." Currently, the only supported kind is "postgresql". For example,
given a Secret "database" with the data

	PGHOST=db
	PGDATABASE=clair
	PGUSER=clair
	PGPASSWORD=password
	PGSSLMODE=disable

and a URI of "database+postgresql:secret:database", the resulting string
would be:

	postresql://clair:password@db/clair?sslmode=disable

The environment variables documented in
https://www.postgresql.org/docs/current/libpq-envars.html are implemented, as
much as makes sense. See LibpqVars for the mapping used.

service

The "service" scheme constructs a URI for the named service. The "portname" and
"scheme" parameters control the port name looked up and the scheme of the
returned URI. The default "portname" is "api" and the default scheme is "http".
For example, given a service "api-a" with a port "api" on port 80,
"service:api-a" becomes "http://api-a.ns.svc/". Given a service "api-b" with a
port "https" on port 8443, "service:api-b?portname=https&scheme=https" becomes
"https://api-b.ns.svc:8443/".

indexer

The "indexer" scheme becomes a reference to an indexer service, controlled by
the annotations on the configuration object. There is no authority, path, or
parameters, e.g. "indexer:" becomes "http://indexer.ns.svc/".

matcher

The "matcher" scheme becomes a reference to an matcher service, controlled by
the annotations on the configuration object. There is no authority, path, or
parameters.

notifier

The "notifier" scheme becomes a reference to an notifier service, controlled by
the annotations on the configuration object. There is no authority, path, or
parameters.
*/
package v1alpha1

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"net"
	"net/url"
	"strconv"

	"github.com/go-logr/logr"
	"gopkg.in/yaml.v3"
	corev1 "k8s.io/api/core/v1"
)

// Tmpl is output and a list of warnings.
type tmpl struct {
	bytes.Buffer
	ws []string
}

func (t *tmpl) warn(msg string) {
	t.ws = append(t.ws, msg)
}

// TemplateV1 does the templating for V1 configs.
func (m *ConfigMutator) templateV1(ctx context.Context, tmpl *tmpl, in []byte, d *configDetails) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	log := logr.FromContext(ctx)

	var n yaml.Node
	if err := yaml.Unmarshal(in, &n); err != nil {
		return err
	}

	// Look for references and dereference them.
	//
	// This is done with the recursive function `examine`, which takes every
	// string node in the document, runs it through the `resolve` function, and
	// then may modify the string node, based on the output.
	var examine func(n *yaml.Node) error
	resolve := func(in string) (string, error) { return m.resolveURIs(ctx, d, in) }
	examine = func(n *yaml.Node) error {
		if n.Kind == yaml.ScalarNode && n.ShortTag() == `!!str` {
			in := n.Value
			out, err := resolve(in)
			switch {
			case errors.Is(err, nil):
			case errors.Is(err, errWarning):
				tmpl.warn(err.Error())
			default:
				log.Info("errored templating string", "error", err, "in", in)
				return err
			}
			if out != n.Value {
				log.V(2).Info("setting string",
					"in", in, "out", out)
				n.Value = out
			}
			return nil
		}
		for _, n := range n.Content {
			if err := examine(n); err != nil {
				return err
			}
		}
		return nil
	}
	if err := examine(&n); err != nil {
		return err
	}

	enc := yaml.NewEncoder(tmpl)
	enc.SetIndent(2)
	if err := enc.Encode(&n); err != nil {
		return err
	}
	return nil
}

// ResolveURIs looks for special URIs and then attempts to resolve them in the
// current context.
func (m *ConfigMutator) resolveURIs(ctx context.Context, d *configDetails, in string) (string, error) {
	log := logr.FromContext(ctx)
	oops := newWarnErr // a better name for local use.

	u, err := url.Parse(in)
	if err != nil {
		// Not really an error, just return the input unchanged.
		log.V(2).Error(err, "not a URL")
		return in, nil
	}

	var res string
	as := resolveAsKeys
Scheme:
	// Error checks, so they don't need to be duplicated below:
	switch u.Scheme {
	case `secret`:
		if !d.isSecret {
			return in, errors.New(`cannot reference secret from config in non-secret`)
		}
		fallthrough
	case `configmap`, `database+postgresql`, `service`:
		if u.Opaque == "" {
			return in, oops("found malformed %s URI %#q", u.Scheme, u.String())
		}
	case `database`:
		return in, oops("found malformed database URI %#q, missing kind", u.String())
	case `indexer`, `matcher`, `notifier`:
	default:
		log.V(2).Info(`ignoring unsupported scheme`, "scheme", u.Scheme)
		return in, nil
	}

	switch u.Scheme {
	case `secret`, `configmap`:
		var ok bool
		var rd configDetails
		switch u.Scheme {
		case `secret`:
			var sec corev1.Secret
			if err := m.client.Get(ctx, toName(u.Opaque), &sec); err != nil {
				return in, err
			}
			if err := rd.fromSecret(&sec); err != nil {
				return in, err
			}
		case `configmap`:
			var cm corev1.ConfigMap
			if err := m.client.Get(ctx, toName(u.Opaque), &cm); err != nil {
				return in, err
			}
			if err := rd.fromConfigMap(&cm); err != nil {
				return in, err
			}
		}
		res, ok = resolveFromKeys(&rd, u.Query(), as)
		if !ok {
			return in, oops("missing %#q parameter in %s URI %#q", "key", u.Scheme, u.String())
		}
	case `service`:
		v := u.Query()
		name := PortAPI
		if n, ok := v[`portname`]; ok {
			name = n[0]
		}
		var srv corev1.Service
		if err := m.client.Get(ctx, toName(u.Opaque), &srv); err != nil {
			return in, err
		}
		var port *corev1.ServicePort
		for i, p := range srv.Spec.Ports {
			if p.Name == name {
				port = &srv.Spec.Ports[i]
				break
			}
		}
		if port == nil {
			return in, oops("unable to find expected port name %#q in service %#q", name, srv.Name)
		}
		u := url.URL{
			Scheme: `http`,
			Host:   fmt.Sprintf("%s.%s.srv", srv.Name, srv.Namespace),
		}
		if s, ok := v[`scheme`]; ok {
			u.Scheme = s[0]
		}
		switch {
		// The arms of the switch are well-known schemes and ports. Omit the
		// port number if it's the expected one.
		case u.Scheme == `http` && port.Port == 80:
		case u.Scheme == `https` && port.Port == 443:
		default:
			u.Host = net.JoinHostPort(u.Host, strconv.Itoa(int(port.Port)))
		}
		res = u.String()
	case `database+postgresql`:
		// Strip off the "envelope" scheme, mark the argument as a PostgresQL
		// config, and re-enter this switch.
		su, err := url.Parse(u.Opaque)
		if err != nil {
			return in, oops("found malformed database URI %#q", u.String()).err(err)
		}
		u = su
		as = resolveAsPostgres
		goto Scheme
	case `indexer`, `matcher`, `notifier`:
		// Construct a service URI and then do a recursive call.
		var key string
		switch u.Scheme {
		case `indexer`:
			key = TemplateIndexerService
		case `matcher`:
			key = TemplateMatcherService
		case `notifier`:
			key = TemplateNotifierService
		}
		n, ok := d.annotations[key]
		if !ok {
			return in, oops(`scheme %#q used, but annotation not present`, u.Scheme)
		}
		su, err := url.Parse(`service:` + n)
		if err != nil {
			panic("programmer error: couldn't construct service URI")
		}
		su.RawQuery = u.RawQuery
		return m.resolveURIs(ctx, d, su.String())
	}

	return res, nil
}

// ResolveFromKeys takes a configDetails (a generalized ConfigMap or Secret) and
// interprets them according to "how".
//
// If using the default "keys" scheme, the "key" and "join" members of the
// url.Values are used to construct a return.
//
// If using the "postgres" scheme, the configDetails is interpreted using
// resolvePostgres.
func resolveFromKeys(d *configDetails, v url.Values, how resolveAs) (string, bool) {
	var out string
	switch how {
	case resolveAsPostgres:
		out = resolvePostgres(d.strData, d.data).String()
	case resolveAsKeys:
		ks, ok := v["key"]
		if !ok {
			return "", false
		}
		vs := make([][]byte, 0, len(ks))
		for _, k := range ks {
			if x, ok := d.item(k); ok {
				vs = append(vs, x)
			}
		}
		out = string(bytes.Join(vs, []byte(v.Get("join"))))
	default:
		panic("programmer error")
	}
	return out, true
}

type resolveAs uint

const (
	resolveAsKeys resolveAs = iota
	resolveAsPostgres
)

// WarnErr is an error that should be exposed as a warning to a user-facing log
// system.
type warnErr struct {
	next error
	warn string
}

func newWarnErr(f string, v ...interface{}) *warnErr {
	return &warnErr{warn: fmt.Sprintf(f, v...)}
}

func (w *warnErr) err(e error) *warnErr {
	w.next = e
	return w
}

func (w *warnErr) Error() string {
	if w.next == nil {
		return w.warn
	}
	return fmt.Sprintf("%s: %v", w.warn, w.next)
}

func (w *warnErr) Warning() string {
	return w.warn
}

func (w *warnErr) Unwrap() error {
	return w.next
}

func (w *warnErr) Is(e error) bool {
	return w == e || e == errWarning
}

var (
	// Static check
	_ error = (*warnErr)(nil)
	// Sentinel for use with errors.Is
	errWarning = errors.New("warning")
)

// ResolvePostgres looks at the keys in the provided maps and interprets them as
// a bunch of libpq environment variables.
func resolvePostgres(d map[string]string, b map[string][]byte) *url.URL {
	out := struct {
		Host, Port, Database, User, Password string
	}{}
	vs := url.Values{}
	for k, q := range LibpqVars {
		x := d[k]
		if x == "" {
			v, ok := b[k]
			if !ok {
				continue
			}
			x = string(v)
		}
		switch k {
		case "PGHOST":
			out.Host = x
		case "PGPORT":
			out.Port = x
		case "PGDATABASE":
			out.Database = x
		case "PGUSER":
			out.User = x
		case "PGPASSWORD":
			out.Password = x
		default:
			vs.Set(q, x)
		}
	}
	ou := url.URL{
		Scheme:   `postgresql`,
		Host:     out.Host,
		User:     url.UserPassword(out.User, out.Password),
		Path:     "/" + out.Database,
		RawQuery: vs.Encode(),
	}
	if out.Port != "" {
		ou.Host = net.JoinHostPort(ou.Host, out.Port)
	}
	return &ou
}

// LibpqVars specifies how the templating code maps keys to URI parameters.
//
// "PGSERVICEFILE" is unimplemented because it cannot be specified in the
// connection string.
//
// "PGREQUIRESSL" is unimplemented because it is superseded by "PGSSLMODE".
//
// Not all of the options are supported by Clair's database driver, so check the
// documentation if attempting to use the more esoteric options.
//
// See also https://www.postgresql.org/docs/current/libpq-envars.html
var LibpqVars = map[string]string{
	"PGHOST":                  "",         // Added as the authority section.
	"PGPORT":                  "",         // Added as the port onto the authority section.
	"PGDATABASE":              "",         // Added as the path of the URI.
	"PGUSER":                  "",         // Added as the userinfo of the URI.
	"PGPASSWORD":              "",         // Added as the userinfo of the URI.
	"PGPASSFILE":              "passfile", // Note that this will end up referring to a file in the running container.
	"PGCHANNELBINDING":        "channel_binding",
	"PGSERVICE":               "service",
	"PGOPTIONS":               "options",
	"PGAPPNAME":               "application_name",
	"PGSSLMODE":               "sslmode",
	"PGSSLCOMPRESSION":        "sslcompression",
	"PGSSLCERT":               "sslcert",
	"PGSSLKEY":                "sslkey",
	"PGSSLROOTCERT":           "sslrootcert",
	"PGSSLCRL":                "sslcrl",
	"PGREQUIREPEER":           "requirepeer",
	"PGSSLMINPROTOCOLVERSION": "ssl_min_protocol_version",
	"PGSSLMAXPROTOCOLVERSION": "ssl_max_protocol_version",
	"PGGSSENCMODE":            "gssencmode",
	"PGKRBSRVNAME":            "krbsrvname",
	"PGGSSLIB":                "gsslib",
	"PGCONNECT_TIMEOUT":       "connect_timeout",
	"PGCLIENTENCODING":        "client_encoding",
	"PGTARGETSESSIONATTRS":    "target_session_attrs",
}

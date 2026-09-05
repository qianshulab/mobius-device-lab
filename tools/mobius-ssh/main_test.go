package main

import (
	"crypto/ed25519"
	"crypto/rand"
	"errors"
	"net"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/pkg/sftp"
	"golang.org/x/crypto/ssh"
)

func TestParseGeneratedPasswordSSHCommand(t *testing.T) {
	knownHosts := filepath.Join(t.TempDir(), "known hosts")
	args := []string{
		"-T", "-F", "none", "-p", "2222",
		"-o", "StrictHostKeyChecking=accept-new",
		"-o", "UserKnownHostsFile=" + knownHosts,
		"-o", "ConnectTimeout=8",
		"-o", "ServerAliveInterval=15",
		"-o", "ServerAliveCountMax=2",
		"-o", "LogLevel=ERROR",
		"-o", "BatchMode=no",
		"-o", "IdentitiesOnly=yes",
		"-o", "PubkeyAuthentication=no",
		"-o", "PreferredAuthentications=password",
		"-o", "PasswordAuthentication=yes",
		"-o", "KbdInteractiveAuthentication=no",
		"-o", "NumberOfPasswordPrompts=1",
		"root@192.168.1.20", "printf 'ready now'",
	}
	opts, err := parseSSHArgs(args)
	if err != nil {
		t.Fatal(err)
	}
	if opts.username != "root" || opts.host != "192.168.1.20" || opts.port != 2222 {
		t.Fatalf("unexpected endpoint: %#v", opts)
	}
	if !opts.passwordEnabled || opts.knownHostsPath != knownHosts || opts.command != "printf 'ready now'" {
		t.Fatalf("unexpected password command: %#v", opts)
	}
}

func TestParseSCPPreservesRemoteSpacesAndApostrophes(t *testing.T) {
	knownHosts := filepath.Join(t.TempDir(), "known_hosts")
	remote := "root@192.168.1.20:/var/mobile/a folder/it's @here.ipa"
	opts, source, destination, err := parseSCPArgs([]string{
		"-s", "-F", "none", "-P", "22",
		"-o", "StrictHostKeyChecking=accept-new",
		"-o", "UserKnownHostsFile=" + knownHosts,
		"-o", "ConnectTimeout=8",
		"-o", "LogLevel=ERROR",
		"-o", "BatchMode=no",
		"-o", "IdentitiesOnly=yes",
		"-o", "PubkeyAuthentication=no",
		"-o", "PreferredAuthentications=password",
		"-o", "PasswordAuthentication=yes",
		"-o", "KbdInteractiveAuthentication=no",
		"-o", "NumberOfPasswordPrompts=1",
		remote, "/tmp/download.ipa",
	})
	if err != nil {
		t.Fatal(err)
	}
	parsed, ok := parseRemoteSpec(source)
	if !ok || parsed.path != "/var/mobile/a folder/it's @here.ipa" {
		t.Fatalf("remote path was changed: %#v", parsed)
	}
	if destination != "/tmp/download.ipa" || !opts.passwordEnabled {
		t.Fatalf("unexpected SCP parse: %q %#v", destination, opts)
	}
}

func TestCompatibilityAlgorithmsStayBounded(t *testing.T) {
	if !contains(hostKeyAlgorithms(), ssh.KeyAlgoRSA) {
		t.Fatal("legacy RSA host keys should remain available for jailbreak devices")
	}
	if !contains(keyExchanges(), ssh.InsecureKeyExchangeDH14SHA1) {
		t.Fatal("group14-sha1 compatibility KEX should remain available")
	}
	if contains(keyExchanges(), ssh.InsecureKeyExchangeDH1SHA1) {
		t.Fatal("group1 KEX must not be enabled")
	}
	if contains(hostKeyAlgorithms(), ssh.InsecureKeyAlgoDSA) {
		t.Fatal("DSA host keys must not be enabled")
	}
	for _, cipher := range ssh.SupportedAlgorithms().Ciphers {
		if strings.Contains(cipher, "cbc") {
			t.Fatalf("CBC cipher unexpectedly entered the secure set: %s", cipher)
		}
	}
}

func contains(values []string, expected string) bool {
	for _, value := range values {
		if value == expected {
			return true
		}
	}
	return false
}

func TestRejectsNonLocalHostAndNonLoopbackForward(t *testing.T) {
	accepted := []string{
		"127.0.0.1",
		"10.0.0.1",
		"172.16.0.1",
		"172.31.255.254",
		"192.168.0.1",
		"169.254.1.1",
	}
	for _, host := range accepted {
		if err := validateEndpoint("root", host); err != nil {
			t.Errorf("local address %s was rejected: %v", host, err)
		}
	}
	rejected := []string{
		"8.8.8.8",
		"100.64.0.1",
		"172.15.255.255",
		"172.32.0.1",
		"198.18.0.1",
		"203.0.113.10",
	}
	for _, host := range rejected {
		if err := validateEndpoint("root", host); err == nil {
			t.Errorf("non-private address %s must not be accepted", host)
		}
	}
	if _, err := parseForward("0.0.0.0:8080:127.0.0.1:22"); err == nil {
		t.Fatal("non-loopback listener must not be accepted")
	}
	if _, err := parseForward("127.0.0.1:8080:10.0.0.1:22"); err == nil {
		t.Fatal("non-loopback forwarding destination must not be accepted")
	}
}

func TestAcceptNewHostKeyPinsAndRejectsChanges(t *testing.T) {
	knownHosts := filepath.Join(t.TempDir(), "known_hosts")
	if err := os.WriteFile(knownHosts, nil, 0o600); err != nil {
		t.Fatal(err)
	}
	first := newTestSigner(t)
	second := newTestSigner(t)
	remote := &net.TCPAddr{IP: net.ParseIP("192.168.1.20"), Port: 22}
	callback, err := acceptNewHostKey(knownHosts, "")
	if err != nil {
		t.Fatal(err)
	}
	if err := callback("192.168.1.20:22", remote, first.PublicKey()); err != nil {
		t.Fatalf("first-seen host key was not accepted: %v", err)
	}
	contents, err := os.ReadFile(knownHosts)
	if err != nil || !strings.Contains(string(contents), first.PublicKey().Type()) {
		t.Fatalf("host key was not persisted: %v %q", err, contents)
	}
	callback, err = acceptNewHostKey(knownHosts, "")
	if err != nil {
		t.Fatal(err)
	}
	if err := callback("192.168.1.20:22", remote, first.PublicKey()); err != nil {
		t.Fatalf("pinned host key was rejected: %v", err)
	}
	if err := callback("192.168.1.20:22", remote, second.PublicKey()); err == nil {
		t.Fatal("changed host key must be rejected")
	}
}

func TestHostKeyAliasIsPinnedIndependentlyOfForwardPort(t *testing.T) {
	knownHosts := filepath.Join(t.TempDir(), "known_hosts")
	if err := os.WriteFile(knownHosts, nil, 0o600); err != nil {
		t.Fatal(err)
	}
	signer := newTestSigner(t)
	callback, err := acceptNewHostKey(knownHosts, "mobius-usb-device-1")
	if err != nil {
		t.Fatal(err)
	}
	remote := &net.TCPAddr{IP: net.ParseIP("127.0.0.1"), Port: 49152}
	if err := callback("127.0.0.1:49152", remote, signer.PublicKey()); err != nil {
		t.Fatalf("alias host key was not accepted: %v", err)
	}
	contents, err := os.ReadFile(knownHosts)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(string(contents), "mobius-usb-device-1 ") {
		t.Fatalf("unexpected aliased known-hosts line: %q", contents)
	}

	callback, err = acceptNewHostKey(knownHosts, "mobius-usb-device-1")
	if err != nil {
		t.Fatal(err)
	}
	remote = &net.TCPAddr{IP: net.ParseIP("127.0.0.1"), Port: 51000}
	if err := callback("127.0.0.1:51000", remote, signer.PublicKey()); err != nil {
		t.Fatalf("same alias should survive a new USB forward port: %v", err)
	}
}

func TestConcurrentFirstUseCannotPinTwoKeys(t *testing.T) {
	knownHosts := filepath.Join(t.TempDir(), "known_hosts")
	if err := os.WriteFile(knownHosts, nil, 0o600); err != nil {
		t.Fatal(err)
	}
	first := newTestSigner(t)
	second := newTestSigner(t)
	firstCallback, err := acceptNewHostKey(knownHosts, "mobius-usb-device-1")
	if err != nil {
		t.Fatal(err)
	}
	secondCallback, err := acceptNewHostKey(knownHosts, "mobius-usb-device-1")
	if err != nil {
		t.Fatal(err)
	}
	remote := &net.TCPAddr{IP: net.ParseIP("127.0.0.1"), Port: 49152}
	start := make(chan struct{})
	results := make(chan error, 2)
	go func() {
		<-start
		results <- firstCallback("127.0.0.1:49152", remote, first.PublicKey())
	}()
	go func() {
		<-start
		results <- secondCallback("127.0.0.1:49152", remote, second.PublicKey())
	}()
	close(start)
	accepted := 0
	for range 2 {
		if err := <-results; err == nil {
			accepted++
		}
	}
	if accepted != 1 {
		t.Fatalf("exactly one first-use key should be pinned, accepted %d", accepted)
	}
}

func TestPasswordComesFromOneShotLoopbackBroker(t *testing.T) {
	listener, err := net.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	t.Setenv(brokerMarkerEnv, "1")
	_, port, err := net.SplitHostPort(listener.Addr().String())
	if err != nil {
		t.Fatal(err)
	}
	t.Setenv(brokerPortEnv, port)
	token := strings.Repeat("a", brokerTokenLength)
	t.Setenv(brokerTokenEnv, token)
	go func() {
		connection, acceptErr := listener.Accept()
		if acceptErr != nil {
			return
		}
		defer connection.Close()
		_ = connection.SetDeadline(time.Now().Add(2 * time.Second))
		request := make([]byte, len(token)+1)
		_, _ = ioReadFull(connection, request)
		if string(request) == token+"\n" {
			_, _ = connection.Write([]byte("test password"))
		}
	}()
	password, err := readBrokerPassword()
	if err != nil {
		t.Fatal(err)
	}
	defer overwrite(password)
	if string(password) != "test password" {
		t.Fatalf("unexpected broker secret: %q", password)
	}
	if os.Getenv(brokerTokenEnv) != "" || os.Getenv(brokerPortEnv) != "" {
		t.Fatal("broker capability remained in the helper environment")
	}
	if _, err := readBrokerPassword(); err == nil {
		t.Fatal("one-time broker capability was reusable")
	}
}

func TestPasswordBrokerCannotBeRedirectedByLegacyEndpoint(t *testing.T) {
	t.Setenv(brokerMarkerEnv, "1")
	t.Setenv(brokerPortEnv, "")
	t.Setenv(legacyEndpointEnv, "8.8.8.8:22")
	t.Setenv(brokerTokenEnv, strings.Repeat("c", brokerTokenLength))
	if _, err := readBrokerPassword(); err == nil || !strings.Contains(err.Error(), "invalid password broker port") {
		t.Fatalf("legacy endpoint unexpectedly affected the broker: %v", err)
	}
	if os.Getenv(legacyEndpointEnv) != "" || os.Getenv(brokerTokenEnv) != "" {
		t.Fatal("rejected broker state remained in the helper environment")
	}
}

func TestPasswordExecAndSFTPAgainstLocalSSHServer(t *testing.T) {
	address, closeServer := startTestSSHServer(t, "test password")
	defer closeServer()
	host, portText, err := net.SplitHostPort(address)
	if err != nil {
		t.Fatal(err)
	}
	port, err := parsePort(portText)
	if err != nil {
		t.Fatal(err)
	}
	knownHosts := filepath.Join(t.TempDir(), "known_hosts")
	if err := os.WriteFile(knownHosts, nil, 0o600); err != nil {
		t.Fatal(err)
	}
	startTestPasswordBroker(t, "test password")
	opts := defaultOptions()
	opts.username = "root"
	opts.host = host
	opts.port = port
	opts.knownHostsPath = knownHosts
	opts.passwordEnabled = true
	client, err := connect(opts)
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()

	session, err := client.NewSession()
	if err != nil {
		t.Fatal(err)
	}
	output, err := session.CombinedOutput("printf 'MOBIUS_TEST_READY'")
	if err != nil || string(output) != "MOBIUS_TEST_READY" {
		t.Fatalf("unexpected exec result: %v %q", err, output)
	}

	sftpClient, err := sftp.NewClient(client)
	if err != nil {
		t.Fatal(err)
	}
	defer sftpClient.Close()
	workspace := t.TempDir()
	localSource := filepath.Join(workspace, "source file.txt")
	remotePath := filepath.Join(workspace, "remote it's @here.txt")
	localDestination := filepath.Join(workspace, "downloaded file.txt")
	if err := os.WriteFile(localSource, []byte("mobius-sftp-smoke"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := uploadFile(sftpClient, localSource, remotePath); err != nil {
		t.Fatal(err)
	}
	if err := downloadFile(sftpClient, remotePath, localDestination); err != nil {
		t.Fatal(err)
	}
	contents, err := os.ReadFile(localDestination)
	if err != nil || string(contents) != "mobius-sftp-smoke" {
		t.Fatalf("unexpected SFTP round trip: %v %q", err, contents)
	}
}

func startTestPasswordBroker(t *testing.T, password string) {
	t.Helper()
	listener, err := net.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })
	t.Setenv(brokerMarkerEnv, "1")
	_, port, err := net.SplitHostPort(listener.Addr().String())
	if err != nil {
		t.Fatal(err)
	}
	t.Setenv(brokerPortEnv, port)
	token := strings.Repeat("b", brokerTokenLength)
	t.Setenv(brokerTokenEnv, token)
	go func() {
		connection, acceptErr := listener.Accept()
		if acceptErr != nil {
			return
		}
		defer connection.Close()
		_ = connection.SetDeadline(time.Now().Add(2 * time.Second))
		request := make([]byte, len(token)+1)
		_, _ = ioReadFull(connection, request)
		if string(request) == token+"\n" {
			_, _ = connection.Write([]byte(password))
		}
	}()
}

func startTestSSHServer(t *testing.T, password string) (string, func()) {
	t.Helper()
	hostSigner := newTestSigner(t)
	configuration := &ssh.ServerConfig{
		PasswordCallback: func(metadata ssh.ConnMetadata, supplied []byte) (*ssh.Permissions, error) {
			if metadata.User() == "root" && string(supplied) == password {
				return nil, nil
			}
			return nil, errors.New("authentication rejected")
		},
	}
	configuration.AddHostKey(hostSigner)
	listener, err := net.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	go func() {
		connection, acceptErr := listener.Accept()
		if acceptErr != nil {
			return
		}
		server, channels, requests, handshakeErr := ssh.NewServerConn(connection, configuration)
		if handshakeErr != nil {
			_ = connection.Close()
			return
		}
		defer server.Close()
		go ssh.DiscardRequests(requests)
		for incoming := range channels {
			if incoming.ChannelType() != "session" {
				_ = incoming.Reject(ssh.UnknownChannelType, "session required")
				continue
			}
			channel, channelRequests, channelErr := incoming.Accept()
			if channelErr != nil {
				continue
			}
			go serveTestSSHChannel(channel, channelRequests)
		}
	}()
	return listener.Addr().String(), func() { _ = listener.Close() }
}

func serveTestSSHChannel(channel ssh.Channel, requests <-chan *ssh.Request) {
	defer channel.Close()
	for request := range requests {
		switch request.Type {
		case "exec":
			var payload struct{ Command string }
			if err := ssh.Unmarshal(request.Payload, &payload); err != nil || payload.Command != "printf 'MOBIUS_TEST_READY'" {
				_ = request.Reply(false, nil)
				return
			}
			_ = request.Reply(true, nil)
			_, _ = channel.Write([]byte("MOBIUS_TEST_READY"))
			_, _ = channel.SendRequest("exit-status", false, ssh.Marshal(struct{ Status uint32 }{0}))
			return
		case "subsystem":
			var payload struct{ Name string }
			if err := ssh.Unmarshal(request.Payload, &payload); err != nil || payload.Name != "sftp" {
				_ = request.Reply(false, nil)
				return
			}
			_ = request.Reply(true, nil)
			server, err := sftp.NewServer(channel)
			if err != nil {
				return
			}
			_ = server.Serve()
			_ = server.Close()
			return
		default:
			_ = request.Reply(false, nil)
		}
	}
}

func ioReadFull(connection net.Conn, value []byte) (int, error) {
	read := 0
	for read < len(value) {
		count, err := connection.Read(value[read:])
		read += count
		if err != nil {
			return read, err
		}
	}
	return read, nil
}

func newTestSigner(t *testing.T) ssh.Signer {
	t.Helper()
	_, privateKey, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	signer, err := ssh.NewSignerFromKey(privateKey)
	if err != nil {
		t.Fatal(err)
	}
	return signer
}

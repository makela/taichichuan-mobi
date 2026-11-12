# Runs nix/module.nix in a NixOS virtual machine: two instances start, serve
# the page with their own addresses, and the sandbox holds. Run with
# `nix flake check` or `nix build .#checks.x86_64-linux.module -L`.
{ pkgs, self }:
(pkgs.testers.runNixOSTest {
  name = "taichichuan-mobi";
  nodes.machine = {
    imports = [ self.nixosModules.default ];
    services.taichichuan-mobi.instances = {
      www.package = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      test = {
        package = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        port = 3101;
        siteUrl = "https://test.taichichuan.mobi";
      };
      off = {
        enable = false;
        package = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        port = 3102;
      };
    };
    environment.systemPackages = [ pkgs.curl ];
  };
  testScript = ''
    import re

    machine.wait_for_unit("taichichuan-mobi-www.service")
    machine.wait_for_unit("taichichuan-mobi-test.service")
    machine.wait_for_open_port(3100)
    machine.wait_for_open_port(3101)

    with subtest("site serves the page in both languages"):
        machine.succeed("curl -sf 'http://127.0.0.1:3100/?lang=en' | grep -q 'Online Tai Chi Classes'")
        machine.succeed("curl -sf 'http://127.0.0.1:3100/?lang=fi' | grep -q 'Taijiopetusta'")
        machine.succeed("curl -sf -H 'Accept-Language: fi' http://127.0.0.1:3100/ | grep -q '<html lang=\"fi\">'")

    with subtest("security headers are sent"):
        headers = machine.succeed("curl -sfI http://127.0.0.1:3100/")
        assert re.search(r"(?im)^strict-transport-security: max-age=31536000", headers), headers
        assert re.search(r"(?im)^x-content-type-options: nosniff", headers), headers
        assert re.search(r"(?im)^content-security-policy: default-src 'none'", headers), headers

    with subtest("static files are served from the package"):
        page = machine.succeed("curl -sf http://127.0.0.1:3100/")
        styles = re.search(r'href="(/static/styles.css\?v=[0-9a-f]{16})"', page)
        assert styles is not None, page
        machine.succeed(f"curl -sfI 'http://127.0.0.1:3100{styles.group(1)}' | grep -qi 'cache-control: public, max-age=31536000, immutable'")
        machine.succeed("curl -sf http://127.0.0.1:3100/static/styles.css | grep -q 'max-width'")
        # Saved first, as head would close a pipe before curl is done
        machine.succeed("curl -sf -o /tmp/font.woff2 http://127.0.0.1:3100/static/fonts/nunito-latin.woff2")
        machine.succeed("head -c4 /tmp/font.woff2 | grep -q wOF2")
        machine.succeed("curl -sf -o /dev/null http://127.0.0.1:3100/favicon.ico")
        machine.succeed("test \"$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:3100/missing)\" = 404")

    with subtest("each instance has its own address"):
        machine.succeed("curl -sf http://127.0.0.1:3100/ | grep -q 'https://www.taichichuan.mobi/?lang=en'")
        test = machine.succeed("curl -sf http://127.0.0.1:3101/")
        assert "https://test.taichichuan.mobi/?lang=en" in test, test
        assert "www.taichichuan.mobi" not in test, test

    with subtest("a disabled instance does not run"):
        machine.fail("systemctl cat taichichuan-mobi-off.service")
        machine.fail("ss -ltnH | grep -q ':3102 '")

    with subtest("instances only listen locally"):
        machine.succeed("ss -ltnH | grep -q '127.0.0.1:3100 '")
        machine.fail("ss -ltnH | grep -q '0.0.0.0:3100 '")

    with subtest("services stop cleanly"):
        machine.succeed("systemctl stop taichichuan-mobi-test.service")
        machine.succeed("journalctl -b --no-pager -u taichichuan-mobi-test.service | grep -q 'HTTP server stopped'")

    with subtest("no sandbox violations"):
        machine.fail("journalctl -b --no-pager | grep -E 'type=1326|SIGSYS|Bad system call'")
        machine.fail("journalctl -b --no-pager -u 'taichichuan-mobi-*' | grep -iE 'error|panic|denied'")
        machine.succeed("test -z \"$(systemctl list-units --failed --plain --no-legend 'taichichuan-mobi-*')\"")
  '';
}).overrideTestDerivation (_: {
  # Also on machines without KVM, where QEMU emulates the CPU: slower, a few
  # minutes, but it works. QEMU still uses KVM when there is one.
  requiredSystemFeatures = [ "nixos-test" ];
})

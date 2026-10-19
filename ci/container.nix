# The container image around a statically linked executable (the flake's static executable, or
# the one `just static-native` built, through ci/container.sh): the executable as
# /dijo with nothing else, run as an unprivileged user that may write only to /tmp and its
# cache, serving on port 9998. Building it runs the executable once, so an executable for another
# machine or one that needs a shared library fails here.
{ pkgs, executable }:
pkgs.dockerTools.buildLayeredImage {
  name = "dijo";
  tag = "local";
  fakeRootCommands = ''
    cp ${executable} dijo
    mkdir -p tmp var/cache/dijo
    chmod 1777 tmp
    chown 10001:10001 var/cache/dijo
    printf 'probe\n' > "$TMPDIR/probe.txt"
    XDG_CACHE_HOME="$TMPDIR" ./dijo detect "$TMPDIR/probe.txt"
  '';
  config = {
    Entrypoint = [ "/dijo" ];
    Cmd = [ "serve" ];
    User = "10001:10001";
    Env = [
      "XDG_CACHE_HOME=/var/cache"
      "DIJO_LISTEN=0.0.0.0:9998"
    ];
    ExposedPorts."9998/tcp" = { };
  };
}

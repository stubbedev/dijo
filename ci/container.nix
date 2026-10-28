# The container image around a statically linked executable (the flake's static executable, or
# the one `just static-native` built, through ci/container.sh): the executable as
# /dijo with nothing else, run as an unprivileged user that may write only to /tmp and its
# cache, serving on port 9998, and checked healthy by `dijo health` on the port DIJO_LISTEN
# names. Building it runs the executable once, so an executable for another machine or one that
# needs a shared library fails here.
{ pkgs, executable }:
let
  seconds = n: n * 1000000000;
in
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
    Healthcheck = {
      Test = [ "CMD" "/dijo" "health" ];
      Interval = seconds 30;
      Timeout = seconds 10;
      StartPeriod = seconds 10;
      Retries = 3;
    };
    Labels = {
      "org.opencontainers.image.source" = "https://github.com/stubbedev/dijo";
      "org.opencontainers.image.description" = "Extract text and metadata from documents";
      "org.opencontainers.image.licenses" = "Apache-2.0";
    };
  };
}

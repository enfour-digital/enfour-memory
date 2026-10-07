{ config, lib, pkgs, ... }:
let
  cfg = config.services.enfour-memory;
  docker = "${pkgs.docker}/bin/docker --host unix:///var/run/docker.sock";
  health = pkgs.writeShellScript "enfour-memory-ready" ''
    for attempt in $(${pkgs.coreutils}/bin/seq 1 30); do
      if ${docker} exec enfour-memory /usr/local/bin/enfour-memory health; then exit 0; fi
      sleep 2
    done
    echo "Enfour Memory did not become ready" >&2
    exit 1
  '';
in {
  options.services.enfour-memory = {
    enable = lib.mkEnableOption "Enfour Memory";
    image = lib.mkOption { type = lib.types.str; default = "enfour-memory:0.6.0"; };
    imageFile = lib.mkOption {
      type = lib.types.package;
      description = "Local Docker image archive. The service loads it before each start. No registry access.";
    };
    dataDir = lib.mkOption { type = lib.types.str; default = "/var/lib/enfour-memory"; };
    uid = lib.mkOption { type = lib.types.ints.unsigned; default = 1000; };
    gid = lib.mkOption { type = lib.types.ints.unsigned; default = 100; };
    bindAddress = lib.mkOption { type = lib.types.str; default = "127.0.0.1"; };
    allowedHosts = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ "localhost" "127.0.0.1" ];
    };
  };
  config = lib.mkIf cfg.enable {
    assertions = [ {
      assertion = lib.hasPrefix "/" cfg.dataDir && cfg.dataDir != "/";
      message = "Enfour Memory dataDir must be an absolute directory other than /.";
    } ];
    virtualisation.docker.enable = true;
    virtualisation.oci-containers.backend = "docker";
    virtualisation.oci-containers.containers.enfour-memory = {
      serviceName = "enfour-memory";
      inherit (cfg) image imageFile;
      autoStart = true;
      pull = "never";
      user = "${toString cfg.uid}:${toString cfg.gid}";
      ports = [ "${cfg.bindAddress}:7463:7463" ];
      volumes = [ "${cfg.dataDir}/state:/data" "${cfg.dataDir}/models:/models:ro" "${cfg.dataDir}/language:/language:ro" ];
      environment.TOKENIZERS_PARALLELISM = "false";
      environment.ENFOUR_LANGUAGE = "/language/dictionary.json";
      cmd = [ "serve" "--bind" "0.0.0.0:7463" "--token-file" "/data/access.token"
        "--hosts" (lib.concatStringsSep "," cfg.allowedHosts) ];
      extraOptions = [
        "--read-only" "--cap-drop=ALL" "--security-opt=no-new-privileges:true"
        "--cpus=2" "--memory=3g" "--pids-limit=128" "--tmpfs=/tmp:size=32m,mode=1777"
        "--health-cmd=/usr/local/bin/enfour-memory health" "--health-interval=60s"
        "--health-timeout=10s" "--health-retries=3" "--health-start-period=10s"
      ];
    };
    systemd.services.enfour-memory = {
      description = "Enfour Memory local MCP and graph service";
      requires = [ "docker.service" ];
      wants = [ "network-online.target" ];
      after = [ "network-online.target" ];
      unitConfig.RequiresMountsFor = cfg.dataDir;
      environment.DOCKER_HOST = "unix:///var/run/docker.sock";
      # Provision and migrate data explicitly. Activation never creates a new DB.
      preStart = lib.mkBefore ''
        test -s ${lib.escapeShellArg "${cfg.dataDir}/state/memory.sqlite"}
        test -s ${lib.escapeShellArg "${cfg.dataDir}/state/access.token"}
        test -s ${lib.escapeShellArg "${cfg.dataDir}/models/manifest.json"}
      '';
      serviceConfig = {
        ExecStartPost = health;
        RestartSec = 5;
        TimeoutStartSec = lib.mkForce 180;
      };
    };
  };
}

{
  pkgs,
  foundryvttFetch,
}: let
  moduleOne = pkgs.runCommand "foundry-test-module-v1" {} ''
    mkdir -p "$out"
    printf 'v1\n' > "$out/content.txt"
  '';
  moduleTwo = pkgs.runCommand "foundry-test-module-v2" {} ''
    mkdir -p "$out"
    printf 'v2\n' > "$out/content.txt"
  '';
  systemOne = pkgs.runCommand "foundry-test-system-v1" {} ''
    mkdir -p "$out"
    printf 'system\n' > "$out/content.txt"
  '';
  manifest = name: packages:
    pkgs.writeText name (builtins.toJSON {
      schemaVersion = 1;
      inherit packages;
    });
  initial = manifest "foundry-initial.json" [
    {
      kind = "module";
      id = "demo";
      state = "present";
      version = "1";
      storePath = moduleOne;
    }
    {
      kind = "system";
      id = "rules";
      state = "present";
      version = "1";
      storePath = systemOne;
    }
  ];
  upgraded = manifest "foundry-upgraded.json" [
    {
      kind = "module";
      id = "demo";
      state = "present";
      version = "2";
      storePath = moduleTwo;
    }
  ];
  removed = manifest "foundry-removed.json" [
    {
      kind = "module";
      id = "demo";
      state = "absent";
      version = "";
      storePath = "";
    }
  ];
in
  pkgs.testers.runNixOSTest {
    name = "foundry-declarative-reconcile";
    nodes.machine = {
      environment.systemPackages = [foundryvttFetch];
    };
    testScript = ''
      machine.start()
      machine.succeed("mkdir -p /var/lib/foundryvtt/Data/worlds/kept && printf world > /var/lib/foundryvtt/Data/worlds/kept/world.json")
      machine.succeed("mkdir -p /var/lib/foundryvtt/Data/systems/rules && printf manual > /var/lib/foundryvtt/Data/systems/rules/content.txt")
      machine.succeed("foundryvtt-fetch reconcile --desired ${initial} --data-dir /var/lib/foundryvtt --state-file /var/lib/foundryvtt/.foundry-circle-packages.json")
      machine.succeed("test ! -L /var/lib/foundryvtt/Data/modules/demo")
      machine.succeed("test $(cat /var/lib/foundryvtt/Data/modules/demo/content.txt) = v1")
      machine.succeed("test -w /var/lib/foundryvtt/Data/modules/demo/content.txt")
      machine.succeed("test ! -L /var/lib/foundryvtt/Data/systems/rules")
      machine.succeed("test $(cat /var/lib/foundryvtt/Data/systems/rules/content.txt) = manual")
      machine.succeed("printf manual > /var/lib/foundryvtt/Data/modules/demo/content.txt")
      machine.succeed("foundryvtt-fetch reconcile --desired ${upgraded} --data-dir /var/lib/foundryvtt --state-file /var/lib/foundryvtt/.foundry-circle-packages.json")
      machine.succeed("test $(cat /var/lib/foundryvtt/Data/modules/demo/content.txt) = manual")
      machine.succeed("foundryvtt-fetch reconcile --desired ${initial} --data-dir /var/lib/foundryvtt --state-file /var/lib/foundryvtt/.foundry-circle-packages.json")
      machine.succeed("test $(cat /var/lib/foundryvtt/Data/modules/demo/content.txt) = manual")
      machine.succeed("foundryvtt-fetch reconcile --desired ${removed} --data-dir /var/lib/foundryvtt --state-file /var/lib/foundryvtt/.foundry-circle-packages.json")
      machine.succeed("test -d /var/lib/foundryvtt/Data/modules/demo")
      machine.succeed("test $(cat /var/lib/foundryvtt/Data/modules/demo/content.txt) = manual")
      machine.succeed("foundryvtt-fetch reconcile --desired ${initial} --data-dir /var/lib/foundryvtt --state-file /var/lib/foundryvtt/.foundry-circle-packages.json")
      machine.succeed("rm -rf /var/lib/foundryvtt/Data/modules/demo")
      machine.succeed("foundryvtt-fetch reconcile --desired ${initial} --data-dir /var/lib/foundryvtt --state-file /var/lib/foundryvtt/.foundry-circle-packages.json")
      machine.fail("test -e /var/lib/foundryvtt/Data/modules/demo")
      machine.succeed("foundryvtt-fetch reconcile --desired ${removed} --data-dir /var/lib/foundryvtt --state-file /var/lib/foundryvtt/.foundry-circle-packages.json")
      machine.succeed("foundryvtt-fetch reconcile --desired ${initial} --data-dir /var/lib/foundryvtt --state-file /var/lib/foundryvtt/.foundry-circle-packages.json")
      machine.succeed("test $(cat /var/lib/foundryvtt/Data/modules/demo/content.txt) = v1")
      machine.succeed("test $(cat /var/lib/foundryvtt/Data/worlds/kept/world.json) = world")
    '';
  }

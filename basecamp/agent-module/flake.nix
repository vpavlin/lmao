{
  description = "Logos Agent Module";

  inputs = {
    logos-module-builder.url = "github:logos-co/logos-module-builder";
    nix-bundle-lgx.url = "github:logos-co/nix-bundle-lgx";
  };

  outputs = inputs@{ logos-module-builder, ... }:
    logos-module-builder.lib.mkLogosModule {
      src = ./.;
      configFile = ./metadata.json;
      flakeInputs = inputs;
      preConfigure = ''
        logos-cpp-generator --from-header src/agent_impl.h \
          --backend qt \
          --impl-class AgentImpl \
          --impl-header agent_impl.h \
          --metadata metadata.json \
          --output-dir ./generated_code
      '';
    };
}

// One project exercising every parameterized-command surface. `ping` stands
// in for a dev server: long-running, no port, harmless, ships with Windows.

const base = {
  kind: "generic",
  autostart: false,
  use_dynamic_port: false,
  fixed_port: null,
  env: "",
  role: null,
  dock_window: false,
  play_sound: false,
  dock_headless: false,
  params: [],
};

export function fixtureProjects(root) {
  return [
    {
      id: "e2e-demo",
      name: "e2e-demo",
      root,
      presets: [],
      active_preset: null,
      commands: [
        {
          ...base,
          id: "pinger",
          name: "pinger",
          cmd: "ping {COUNT} 127.0.0.1",
          params: [
            {
              name: "count",
              label: "Count",
              values: [
                { value: "long", label: "Long", flag: "-n 300" },
                { value: "longer", label: "Longer", flag: "-n 600" },
              ],
              last_value: "long",
            },
          ],
        },
        { ...base, id: "ping-a", name: "ping-a", cmd: "ping -n 400 127.0.0.1" },
        { ...base, id: "ping-b", name: "ping-b", cmd: "ping -n 500 127.0.0.1", autostart: false, role: "BE" },
        { ...base, id: "plain", name: "plain", cmd: "ping -n 200 127.0.0.1" },
      ],
    },
  ];
}

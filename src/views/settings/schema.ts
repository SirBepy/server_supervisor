import { defineSchema } from "../../../vendor/tauri_kit/frontend/settings/schema";
import type { SettingsSchema } from "../../../vendor/tauri_kit/frontend/settings/schema";
import { makeTokenField } from "./token-field";

export function buildSettingsSchema(apiToken: string): SettingsSchema {
  return defineSchema({
    sections: [
      {
        title: "AI",
        fields: [
          {
            key: "ai_can_add_commands",
            kind: "toggle",
            label: "Allow AI to create commands",
            tooltip:
              "When enabled, AI agents using the HTTP API can add new commands to existing projects.",
          },
          {
            key: "ai_can_add_projects",
            kind: "toggle",
            label: "Allow AI to create projects",
            tooltip:
              "When enabled, AI agents using the HTTP API can register new projects.",
          },
        ],
      },
      {
        title: "App",
        fields: [
          {
            key: "autostart",
            kind: "toggle",
            label: "Launch at startup",
            tooltip: "Start Server Supervisor automatically when Windows starts.",
          },
          {
            key: "api_port",
            kind: "integer",
            label: "API port",
            min: 1024,
            max: 65535,
            tooltip: "Port the localhost HTTP API binds to. Requires restart to take effect.",
          },
          makeTokenField(apiToken),
        ],
      },
    ],
  });
}

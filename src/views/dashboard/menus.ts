// Kebab popover menus for the dashboard list. cmdMenu/moreMenu render only the
// trigger button; the actual popover floats via portalMenu(), rendered at the
// root of draw() as position:fixed so it always clears every stacking context.

import { html, nothing, type TemplateResult } from "lit-html";
import { styleMap } from "lit-html/directives/style-map.js";
import * as ipc from "../../shared/ipc";
import { ui } from "./state";
import { portUrl } from "./helpers";
import { cmdMenu, cmdMenuContent } from "./menus/cmd-menu";
import { moreMenu, projMenuContent, startAddProjectInGroup } from "./menus/project-menu";
import { groupMenu, groupMenuContent, moveToGroupContent, emptyMenuContent } from "./menus/group-menu";

export {
  cmdMenu,
  cmdMenuContent,
  moreMenu,
  projMenuContent,
  startAddProjectInGroup,
  groupMenu,
  groupMenuContent,
  moveToGroupContent,
  emptyMenuContent,
};

// Exported so Home's port cell and the Project screen's port chip can reuse
// it to open the served page directly (same idiom as copyPortUrl below).
export function openInBrowser(port: number, flutter: boolean) {
  void ipc.openPortUrl(portUrl(port), flutter);
}

// Exported so the Project screen's proxy hub section can reuse it for the
// hub's own copyable address (same clipboard idiom, different port source).
export function copyPortUrl(port: number) {
  void navigator.clipboard?.writeText(portUrl(port));
}

// Store the anchor rect for the portal from a button click event.
export function setButtonAnchor(e: Event, menuHeight = 200) {
  const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
  ui.menuAnchor = {
    top: rect.top,
    bottom: rect.bottom,
    left: rect.left,
    right: rect.right,
    flipUp: rect.bottom + menuHeight > window.innerHeight,
  };
}

// Store the anchor from a contextmenu (mouse) event. Menu opens at the cursor.
export function setMouseAnchor(e: MouseEvent, menuHeight = 200) {
  ui.menuAnchor = {
    top: e.clientY,
    bottom: e.clientY,
    left: e.clientX,
    right: e.clientX,
    flipUp: e.clientY + menuHeight > window.innerHeight,
  };
}

// The floating menu portal. Call from draw() at root level. Renders position:fixed
// at the stored anchor so the menu is always on top of every stacking context.
export function portalMenu(): TemplateResult | typeof nothing {
  const anchor = ui.menuAnchor;
  if (!anchor) return nothing;

  let content: TemplateResult | typeof nothing = nothing;

  if (ui.openMenuFor !== null) {
    const project = ui.projects.find((p) => p.id === ui.openMenuFor);
    if (project) content = projMenuContent(project);
  } else if (ui.openCmdMenuFor !== null) {
    outer: for (const project of ui.projects) {
      for (const cmd of project.commands) {
        const id = `${project.id}:${cmd.id}`;
        if (id === ui.openCmdMenuFor) {
          const status = ui.statusById[id]?.status ?? "stopped";
          content = cmdMenuContent(project, cmd, id, status);
          break outer;
        }
      }
    }
  } else if (ui.openGroupMenuFor !== null) {
    const group = ui.groups.find((g) => g.id === ui.openGroupMenuFor);
    if (group) content = groupMenuContent(group);
  } else if (ui.openMoveToGroupFor !== null) {
    content = moveToGroupContent(ui.openMoveToGroupFor);
  } else if (ui.openEmptyMenu) {
    content = emptyMenuContent();
  }

  if (content === nothing) return nothing;

  const menuWidth = 200;
  const openLeft = anchor.right >= menuWidth;
  const style = styleMap({
    position: "fixed",
    zIndex: "9999",
    right: openLeft ? `${window.innerWidth - anchor.right}px` : undefined,
    left: openLeft ? undefined : `${anchor.left}px`,
    top: anchor.flipUp ? undefined : `${anchor.bottom + 4}px`,
    bottom: anchor.flipUp ? `${window.innerHeight - anchor.top + 4}px` : undefined,
  });

  return html`
    <div class="more-menu" style=${style} @click=${(e: Event) => e.stopPropagation()}>
      ${content}
    </div>
  `;
}

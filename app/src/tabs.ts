export interface TabEl {
  id: string;
  hidden: boolean | string;
  tabIndex: number;
  setAttribute(name: string, value: string): void;
  focus(): void;
}

export interface PanelEl {
  hidden: boolean | string;
}

/// The tab index a key moves to from `current` (arrows wrap, Home and End jump), skipping hidden tabs, or null for a
/// key that is not tab navigation.
export function tabTarget(key: string, current: number, visible: boolean[]): number | null {
  const n = visible.length;
  const step = (from: number, dir: number) => {
    for (let i = 1; i <= n; i++) {
      const j = (((from + dir * i) % n) + n) % n;
      if (visible[j]) return j;
    }
    return current;
  };
  switch (key) {
    case "ArrowRight":
      return step(current, 1);
    case "ArrowLeft":
      return step(current, -1);
    case "Home":
      return step(-1, 1);
    case "End":
      return step(n, -1);
    default:
      return null;
  }
}

/// A WAI-ARIA tablist with automatic activation: one tab is selected and tabbable, and only its panel shows.
export class Tabs {
  private readonly tabs: TabEl[];
  private readonly panels: PanelEl[];
  private readonly onSelect?: (id: string) => void;
  private index = 0;

  constructor(tabs: TabEl[], panels: PanelEl[], onSelect?: (id: string) => void) {
    this.tabs = tabs;
    this.panels = panels;
    this.onSelect = onSelect;
    this.select(tabs[0].id);
  }

  get current(): string {
    return this.tabs[this.index].id;
  }

  select(id: string, focus = false) {
    const i = this.tabs.findIndex((t) => t.id === id && !t.hidden);
    if (i < 0) return;
    this.index = i;
    this.tabs.forEach((t, j) => {
      t.setAttribute("aria-selected", String(j === i));
      t.tabIndex = j === i ? 0 : -1;
      this.panels[j].hidden = j !== i;
    });
    if (focus) this.tabs[i].focus();
    this.onSelect?.(this.tabs[i].id);
  }

  /// The tab showing `panel`.
  tabOf(panel: PanelEl): string | undefined {
    return this.tabs[this.panels.indexOf(panel)]?.id;
  }

  setHidden(id: string, hidden: boolean) {
    const tab = this.tabs.find((t) => t.id === id);
    if (!tab) return;
    tab.hidden = hidden;
    if (hidden && this.current === id) {
      const next = this.tabs.find((t) => !t.hidden);
      if (next) this.select(next.id);
    }
  }

  /// Handles a keydown on the tablist; true when it moved the selection.
  key(key: string): boolean {
    const i = tabTarget(
      key,
      this.index,
      this.tabs.map((t) => !t.hidden),
    );
    if (i === null) return false;
    this.select(this.tabs[i].id, true);
    return true;
  }
}

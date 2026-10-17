// Which group of the settings is open: remembered between starts, and opened
// from elsewhere in the dock (a fault's details open the links).

export type Group = 'autoplay' | 'view' | 'link' | 'model' | 'app'
export const GROUPS: readonly Group[] = ['autoplay', 'view', 'link', 'model', 'app']
const GROUP_KEY = 'akagi.settings.group'

/** The group last looked at, or the first. */
export function savedGroup(): Group {
  try {
    const saved = localStorage.getItem(GROUP_KEY)
    if (saved && (GROUPS as readonly string[]).includes(saved)) return saved as Group
  } catch { /* not remembered */ }
  return 'autoplay'
}
export function rememberGroup(group: Group): void {
  try { localStorage.setItem(GROUP_KEY, group) } catch { /* chosen, not remembered */ }
}

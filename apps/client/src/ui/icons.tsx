// Icons on a 24 px grid with a 1.5 px stroke and round caps, drawn for this client (none are copied from another
// product), and the Zeus mark: a bolt struck on an electrum coin with a pellet border.

const PATHS = {
  download: '<path d="M12 4v11M7 10.5l5 5 5-5M4.5 19.5h15"/>',
  upload: '<path d="M12 16V5M7 9.5l5-5 5 5M4.5 19.5h15"/>',
  link: '<path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1"/><path d="M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1"/>',
  grab: '<path d="M4 6h10M4 11h10M4 16h6"/><path d="M16 13v7M13 17l3 3 3-3"/>',
  globe: '<circle cx="12" cy="12" r="8.5"/><path d="M3.5 12h17M12 3.5c2.5 2.4 3.6 5.2 3.6 8.5s-1.1 6.1-3.6 8.5c-2.5-2.4-3.6-5.2-3.6-8.5S9.5 5.9 12 3.5z"/>',
  queue: '<rect x="4" y="4.5" width="16" height="4" rx="1"/><rect x="4" y="10" width="16" height="4" rx="1"/><rect x="4" y="15.5" width="10" height="4" rx="1"/>',
  clock: '<circle cx="12" cy="12" r="8.5"/><path d="M12 7v5l3 2"/>',
  calendar: '<rect x="4" y="5.5" width="16" height="14" rx="2"/><path d="M4 10h16M8.5 3.5v4M15.5 3.5v4"/>',
  gauge: '<path d="M4.5 16a7.5 7.5 0 1 1 15 0"/><path d="M12 16l3.6-4.4"/><path d="M4 19.5h16"/>',
  search: '<circle cx="11" cy="11" r="6.5"/><path d="m20 20-4.3-4.3"/>',
  folder: '<path d="M3.5 7.5a2 2 0 0 1 2-2h4l2 2.2h7a2 2 0 0 1 2 2V17a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2z"/>',
  folderopen: '<path d="M3.5 17V7.5a2 2 0 0 1 2-2h4l2 2.2h6a2 2 0 0 1 2 2V11"/><path d="M3.5 17l2.4-5.4a1.5 1.5 0 0 1 1.4-.9H20l-2.6 6.6a1.5 1.5 0 0 1-1.4.9H5a1.5 1.5 0 0 1-1.5-1.2z"/>',
  file: '<path d="M6.5 3.5h7l4 4V19a1.5 1.5 0 0 1-1.5 1.5H6.5A1.5 1.5 0 0 1 5 19V5a1.5 1.5 0 0 1 1.5-1.5z"/><path d="M13.5 3.5v4h4"/>',
  doc: '<path d="M6.5 3.5h7l4 4V19a1.5 1.5 0 0 1-1.5 1.5H6.5A1.5 1.5 0 0 1 5 19V5a1.5 1.5 0 0 1 1.5-1.5z"/><path d="M13.5 3.5v4h4M8.5 12h7M8.5 15h7M8.5 9h3"/>',
  archive: '<rect x="3.5" y="4" width="17" height="4.5" rx="1"/><path d="M5 8.5V18a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8.5M10 12.5h4"/>',
  music: '<path d="M9 17.5V6l10-2v11.5"/><circle cx="6.8" cy="17.5" r="2.2"/><circle cx="16.8" cy="15.5" r="2.2"/>',
  program: '<rect x="3.5" y="4.5" width="17" height="15" rx="2"/><path d="M3.5 8.5h17M6 6.5h.01M8.5 6.5h.01M8 13l2 2-2 2M12 17h4"/>',
  video: '<rect x="3.5" y="6" width="12.5" height="12" rx="2"/><path d="m16 10.5 4.5-2.5v8L16 13.5"/>',
  picture: '<rect x="3.5" y="5" width="17" height="14" rx="2"/><circle cx="9" cy="10" r="1.6"/><path d="m4 17 5-4.5 3.5 3 3-2.5 4.5 4"/>',
  box: '<path d="M12 3 20 7.4v9.2L12 21l-8-4.4V7.4z"/><path d="M4 7.4l8 4.4 8-4.4M12 11.8V21"/>',
  check: '<path d="m5 12.5 4.5 4.5L19 7.5"/>',
  x: '<path d="M6 6l12 12M18 6 6 18"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  minus: '<path d="M5 12h14"/>',
  play: '<path d="M7.5 5.2v13.6a.6.6 0 0 0 .9.5l10.8-6.8a.6.6 0 0 0 0-1L8.4 4.7a.6.6 0 0 0-.9.5z"/>',
  pause: '<rect x="6.5" y="5" width="3.6" height="14" rx="1"/><rect x="13.9" y="5" width="3.6" height="14" rx="1"/>',
  stop: '<rect x="6.5" y="6.5" width="11" height="11" rx="2"/>',
  refresh: '<path d="M20 11A8 8 0 0 0 5.6 6.6L4 8"/><path d="M4 3v5h5"/><path d="M4 13a8 8 0 0 0 14.4 4.4L20 16"/><path d="M20 21v-5h-5"/>',
  redo: '<path d="M20 7v5h-5"/><path d="M20 12a8 8 0 1 0-2.3 5.7"/>',
  trash: '<path d="M4 7h16M10 11v6M14 11v6M6 7l1 12a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-12M9 7V4h6v3"/>',
  rename: '<path d="M4 20h4L19 9a2.1 2.1 0 0 0-3-3L5 17v3z"/><path d="m14.5 7.5 3 3"/>',
  compose: '<path d="M4 20h4L19 9a2.1 2.1 0 0 0-3-3L5 17v3z"/><path d="m14.5 7.5 3 3"/>',
  move: '<path d="M5 12h14M15 8l4 4-4 4M9 5H6a1.5 1.5 0 0 0-1.5 1.5v11A1.5 1.5 0 0 0 6 19h3"/>',
  info: '<circle cx="12" cy="12" r="8.5"/><path d="M12 11v5M12 8h.01"/>',
  seal: '<circle cx="12" cy="10" r="6.2"/><path d="M9.4 15.4 8 21.6l4-2 4 2-1.4-6.2"/><path d="M9.6 10l1.6 1.6 3.2-3.2"/>',
  hand: '<path d="M8 13V6.5a1.5 1.5 0 0 1 3 0V12M11 11.5V5a1.5 1.5 0 0 1 3 0v6.5M14 11.5V7a1.5 1.5 0 0 1 3 0v7c0 3.6-2.4 6.5-6 6.5-2.4 0-3.9-1.2-5.2-3.2L4 14.2a1.4 1.4 0 0 1 2.3-1.6L8 15"/>',
  shield: '<path d="M12 3l7.6 2.8v5.6c0 4.8-3.2 8.2-7.6 9.6-4.4-1.4-7.6-4.8-7.6-9.6V5.8z"/><path d="M9 12l2.2 2.2 4-4.2"/>',
  lock: '<rect x="5" y="10.5" width="14" height="10" rx="2"/><path d="M8.5 10.5V8a3.5 3.5 0 0 1 7 0v2.5"/>',
  key: '<circle cx="8" cy="15" r="3.8"/><path d="m10.8 12.3 8.2-8.2M16 7l2.5 2.5M14 9l2 2"/>',
  plug: '<path d="M9 3v5M15 3v5M6 8h12v3a6 6 0 0 1-12 0V8zM12 17v4"/>',
  cloud: '<path d="M7 18.5a4.5 4.5 0 0 1-.6-9 6 6 0 0 1 11.5 1.5 3.8 3.8 0 0 1-.4 7.5z"/>',
  sparkle: '<path d="M12 3l1.8 5.2L19 10l-5.2 1.8L12 17l-1.8-5.2L5 10l5.2-1.8z"/><path d="M18 16l.7 1.8 1.8.7-1.8.7L18 21l-.7-1.8-1.8-.7 1.8-.7z"/>',
  skill: '<path d="M5 4.5h10a2 2 0 0 1 2 2v13l-3-2-3 2-3-2-3 2v-13a2 2 0 0 1 2-2z"/><path d="M8 9h6M8 12.5h4"/>',
  chat: '<path d="M4 5.5A1.5 1.5 0 0 1 5.5 4h13A1.5 1.5 0 0 1 20 5.5v9a1.5 1.5 0 0 1-1.5 1.5H10l-4.5 4v-4A1.5 1.5 0 0 1 4 14.5z"/><path d="M8 9h8M8 12h5"/>',
  gear: '<circle cx="12" cy="12" r="3"/><path d="M12 2.8v2.4M12 18.8v2.4M2.8 12h2.4M18.8 12h2.4M5.5 5.5l1.7 1.7M16.8 16.8l1.7 1.7M5.5 18.5l1.7-1.7M16.8 7.2l1.7-1.7"/><circle cx="12" cy="12" r="6.6"/>',
  sliders: '<path d="M4 6h9M17 6h3M4 12h3M11 12h9M4 18h12"/><circle cx="15" cy="6" r="2"/><circle cx="9" cy="12" r="2"/><circle cx="18" cy="18" r="2"/>',
  palette: '<path d="M12 3a9 9 0 1 0 0 18c1.2 0 2-.8 2-1.8 0-.6-.3-1-.6-1.4-.3-.4-.6-.8-.6-1.3 0-1 .8-1.7 1.8-1.7H17a4 4 0 0 0 4-4C21 6.2 17 3 12 3z"/><circle cx="7.5" cy="11" r="1"/><circle cx="10" cy="7" r="1"/><circle cx="15" cy="7" r="1"/>',
  keyboard: '<rect x="2.5" y="6" width="19" height="12" rx="2"/><path d="M6 10h.01M10 10h.01M14 10h.01M18 10h.01M7 14h10"/>',
  bell: '<path d="M6 17V11a6 6 0 1 1 12 0v6l1.5 2h-15zM10 21h4"/>',
  browser: '<rect x="3" y="4.5" width="18" height="15" rx="2"/><path d="M3 9h18M6 6.8h.01M8.5 6.8h.01"/>',
  drive: '<rect x="3" y="13" width="18" height="6.5" rx="1.5"/><path d="M5 13l2.2-7.2A1.5 1.5 0 0 1 8.6 4.7h6.8a1.5 1.5 0 0 1 1.4 1.1L19 13M16.5 16.3h.01"/>',
  database: '<ellipse cx="12" cy="6" rx="7" ry="2.6"/><path d="M5 6v12c0 1.4 3.1 2.6 7 2.6s7-1.2 7-2.6V6M5 12c0 1.4 3.1 2.6 7 2.6s7-1.2 7-2.6"/>',
  server: '<rect x="4" y="4" width="16" height="7" rx="1.5"/><rect x="4" y="13" width="16" height="7" rx="1.5"/><path d="M7.5 7.5h.01M7.5 16.5h.01"/>',
  bolt: '<path d="M13 2.5 4.8 13.5H11L10 21.5l8.2-11H12z"/>',
  power: '<path d="M12 3.5v8"/><path d="M7 6.5a7 7 0 1 0 10 0"/>',
  star: '<path d="m12 3.8 2.5 5.1 5.6.8-4 4 1 5.6-5.1-2.7-5.1 2.7 1-5.6-4-4 5.6-.8z"/>',
  history: '<path d="M4 12a8 8 0 1 0 2.4-5.7L4 8.5"/><path d="M4 4v4.5h4.5M12 8v4.2l2.8 1.8"/>',
  filter: '<path d="M4 5.5h16l-6.2 7.2v5.5l-3.6 1.8v-7.3z"/>',
  chevron: '<path d="m6 9 6 6 6-6"/>',
  chevronright: '<path d="m9 6 6 6-6 6"/>',
  up: '<path d="M12 19V5M6 11l6-6 6 6"/>',
  down: '<path d="M12 5v14M6 13l6 6 6-6"/>',
  sortasc: '<path d="M7 18V6M3.5 9.5 7 6l3.5 3.5M13 7h7M13 12h5M13 17h3"/>',
  arrowleft: '<path d="M19 12H5M11 6l-6 6 6 6"/>',
  external: '<path d="M14 4h6v6M20 4l-9 9M18 14v4a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4"/>',
  copy: '<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2"/>',
  more: '<circle cx="5.5" cy="12" r="1.2"/><circle cx="12" cy="12" r="1.2"/><circle cx="18.5" cy="12" r="1.2"/>',
  panel: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M9 4v16"/>',
  panelright: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M15 4v16"/>',
  columns: '<rect x="3.5" y="4.5" width="17" height="15" rx="2"/><path d="M9.5 4.5v15M14.5 4.5v15"/>',
  sun: '<circle cx="12" cy="12" r="4"/><path d="M12 2.5v2M12 19.5v2M2.5 12h2M19.5 12h2M5.3 5.3l1.4 1.4M17.3 17.3l1.4 1.4M5.3 18.7l1.4-1.4M17.3 6.7l1.4-1.4"/>',
  moon: '<path d="M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5z"/>',
  contrast: '<circle cx="12" cy="12" r="8.5"/><path d="M12 3.5v17A8.5 8.5 0 0 0 12 3.5z" fill="currentColor"/>',
  send: '<path d="M12 19V5M6 11l6-6 6 6"/>',
  attach: '<path d="m20 11-8.2 8.2a5 5 0 0 1-7-7L13 4a3.3 3.3 0 0 1 4.7 4.7L9.5 17a1.7 1.7 0 0 1-2.4-2.4L14 7.7"/>',
  wrench: '<path d="M14.7 6.3a4 4 0 0 0-5.4 5.2L3.5 17.3a1.8 1.8 0 0 0 2.5 2.5l5.8-5.8a4 4 0 0 0 5.2-5.4l-2.6 2.6-2.1-.5-.5-2.1z"/>',
  eye: '<path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z"/><circle cx="12" cy="12" r="2.8"/>',
  code: '<path d="m8 8-4 4 4 4M16 8l4 4-4 4M13.5 6l-3 12"/>',
  inbox: '<path d="M3.5 13.5 6 5.5h12l2.5 8V18a1.5 1.5 0 0 1-1.5 1.5H5A1.5 1.5 0 0 1 3.5 18z"/><path d="M3.5 13.5H8l1.2 2h5.6l1.2-2h4.5"/>',
  layers: '<path d="M12 3 3 8l9 5 9-5-9-5z"/><path d="m3 12 9 5 9-5M3 16l9 5 9-5"/>',
  wave: '<path d="M3 12c2 0 2-4 4-4s2 8 4 8 2-8 4-8 2 4 4 4"/>',
} as const

export type IconName = keyof typeof PATHS

export function Icon({ name }: { name: IconName }) {
  return <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" dangerouslySetInnerHTML={{ __html: PATHS[name] }} />
}

let markSeq = 0

/** The Zeus mark: an electrum coin with a pellet border and a bolt. */
export function Mark({ title = 'Zeus' }: { title?: string }) {
  const id = `zm${++markSeq}`
  return (
    <svg viewBox="0 0 32 32" role="img" aria-label={title} class="mark">
      <defs>
        <radialGradient id={id} cx=".38" cy=".32" r=".8">
          <stop offset="0" stop-color="#FFF4D2" />
          <stop offset=".55" stop-color="#E2C068" />
          <stop offset="1" stop-color="#A9853A" />
        </radialGradient>
      </defs>
      <circle cx="16" cy="16" r="15" fill={`url(#${id})`} stroke="#7A5C20" stroke-width="1.2" />
      <circle cx="16" cy="16" r="12" fill="none" stroke="#7A5C20" stroke-width=".9" stroke-dasharray=".1 2.35" stroke-linecap="round" />
      <path transform="translate(4 4)" d="M13.6 1.5 4.2 13.4h6.1L8.9 22.5 19.8 9.6h-6.3l1.9-8.1Z" fill="#1B2233" />
    </svg>
  )
}

const VERNE =
  'M 62 517 C 34 470 20 418 20 360 C 20 175 165 20 350 20 C 478 20 562 78 622 152 C 676 220 700 268 742 296 C 790 328 852 340 920 340 C 990 340 1056 320 1104 286 C 1146 256 1170 214 1212 166 C 1290 70 1370 20 1492 20 C 1676 20 1830 172 1830 356 C 1830 548 1680 690 1492 690 C 1384 690 1288 640 1228 556 C 1176 484 1114 442 1035 442 C 938 442 886 510 886 580 C 886 606 893 630 905 652 C 1010 846 1118 1030 1222 1222 C 1248 1270 1260 1324 1260 1378 C 1260 1566 1104 1710 890 1710 C 784 1710 690 1632 632 1530 C 444 1192 254 856 62 517 Z'

/** Verne's loop V, in Verne's gradient: the only place Verne's colours appear on a product surface. */
export function VerneMark() {
  return (
    <svg viewBox="-60 -60 1972 1852" aria-hidden="true">
      <defs>
        <linearGradient id="verne-g" x1="0" y1="0" x2="1852" y2="0" gradientUnits="userSpaceOnUse">
          <stop offset="0" stop-color="#00F5C8" />
          <stop offset=".5" stop-color="#00B4DC" />
          <stop offset="1" stop-color="#005BFF" />
        </linearGradient>
      </defs>
      <path d={VERNE} fill="none" stroke="url(#verne-g)" stroke-width="130" stroke-linejoin="round" />
    </svg>
  )
}

/** The icon for a category or a file type. */
export function categoryIcon(c: string): IconName {
  switch (c) {
    case 'compressed':
      return 'archive'
    case 'documents':
      return 'doc'
    case 'music':
      return 'music'
    case 'programs':
      return 'program'
    case 'video':
      return 'video'
    case 'pictures':
      return 'picture'
    default:
      return 'file'
  }
}

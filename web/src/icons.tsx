// Inline-SVG icon set — a small, cohesive, stroke-based family (Lucide-style:
// 24px grid, 1.5px stroke, round caps/joins, `currentColor`) that replaces every
// emoji in the UI. No icon-font, no dependency. Icons inherit color from the
// surrounding text (`currentColor`) and scale via the `size` prop, so they theme
// for free in dark/light and stay crisp at any DPI.
//
// Accessibility: icons are decorative by default (aria-hidden) and paired with a
// visible text label. Pass `title` to give a standalone icon an accessible name.

import { SVGProps } from "react";

export type IconProps = Omit<SVGProps<SVGSVGElement>, "ref"> & {
  size?: number;
  title?: string;
};

function Svg({ size = 18, title, children, strokeWidth = 1.5, ...rest }: IconProps & { children: React.ReactNode }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={strokeWidth}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden={title ? undefined : true}
      role={title ? "img" : undefined}
      focusable="false"
      {...rest}
    >
      {title ? <title>{title}</title> : null}
      {children}
    </svg>
  );
}

/* --- navigation -------------------------------------------------------------- */

// Live: a 2x2 camera wall.
export const IconLive = (p: IconProps) => (
  <Svg {...p}>
    <rect x="3" y="3" width="7" height="7" rx="1.5" />
    <rect x="14" y="3" width="7" height="7" rx="1.5" />
    <rect x="3" y="14" width="7" height="7" rx="1.5" />
    <rect x="14" y="14" width="7" height="7" rx="1.5" />
  </Svg>
);

// Insights: an axis with a trending line.
export const IconChart = (p: IconProps) => (
  <Svg {...p}>
    <path d="M3 3v18h18" />
    <path d="M7 15l3-4 3 2 4-6" />
  </Svg>
);

export const IconBell = (p: IconProps) => (
  <Svg {...p}>
    <path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9" />
    <path d="M10.3 21a1.94 1.94 0 0 0 3.4 0" />
  </Svg>
);

export const IconHand = (p: IconProps) => (
  <Svg {...p}>
    <path d="M18 11V6a2 2 0 0 0-4 0" />
    <path d="M14 10V4a2 2 0 0 0-4 0v2" />
    <path d="M10 10.5V6a2 2 0 0 0-4 0v8" />
    <path d="M18 8a2 2 0 1 1 4 0v6a8 8 0 0 1-8 8h-2c-2.8 0-4.5-.86-5.99-2.34l-3.6-3.6a2 2 0 0 1 2.83-2.82L7 15" />
  </Svg>
);

export const IconFilm = (p: IconProps) => (
  <Svg {...p}>
    <rect width="18" height="18" x="3" y="3" rx="2" />
    <path d="M7 3v18" />
    <path d="M3 7.5h4" />
    <path d="M3 12h18" />
    <path d="M3 16.5h4" />
    <path d="M17 3v18" />
    <path d="M17 7.5h4" />
    <path d="M17 16.5h4" />
  </Svg>
);

export const IconUser = (p: IconProps) => (
  <Svg {...p}>
    <path d="M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2" />
    <circle cx="12" cy="7" r="4" />
  </Svg>
);

// Stranger / unfamiliar face: a person with a question mark.
export const IconStranger = (p: IconProps) => (
  <Svg {...p}>
    <path d="M10.5 21H5v-2a4 4 0 0 1 4-4h2" />
    <circle cx="10" cy="7" r="4" />
    <path d="M15.6 14.5a2.1 2.1 0 1 1 2.9 2.5c-.6.3-1 .9-1 1.6v.2" />
    <path d="M17.5 21.5v.01" />
  </Svg>
);

export const IconSiren = (p: IconProps) => (
  <Svg {...p}>
    <path d="M7 18v-6a5 5 0 1 1 10 0v6" />
    <path d="M5 21a1 1 0 0 1-1-1v-2a1 1 0 0 1 1-1h14a1 1 0 0 1 1 1v2a1 1 0 0 1-1 1Z" />
    <path d="M21 12h1" />
    <path d="m18.5 4.5-.7.7" />
    <path d="M2 12h1" />
    <path d="M12 2v1" />
    <path d="m5.5 4.5.7.7" />
  </Svg>
);

// Cameras (device management): a video camera.
export const IconVideo = (p: IconProps) => (
  <Svg {...p}>
    <path d="m22 8-6 4 6 4V8Z" />
    <rect width="14" height="12" x="2" y="6" rx="2" />
  </Svg>
);

// A pan-tilt security camera (used for camera-detail / device contexts).
export const IconCctv = (p: IconProps) => (
  <Svg {...p}>
    <path d="M3 7.2 14.4 4l1 3.6L4 11Z" />
    <path d="m15 7 5.5-1.5" />
    <path d="M4 11v4" />
    <path d="M4 15h6" />
    <path d="M10 15v3a2 2 0 0 0 2 2h2" />
    <circle cx="18" cy="18" r="2" />
  </Svg>
);

export const IconSettings = (p: IconProps) => (
  <Svg {...p}>
    <path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z" />
    <circle cx="12" cy="12" r="3" />
  </Svg>
);

// Floor-plan / map (camera placement).
export const IconMap = (p: IconProps) => (
  <Svg {...p}>
    <path d="M9 4 3 6.2v13.6L9 18l6 2 6-2.2V4l-6 2.2L9 4Z" />
    <path d="M9 4v14" />
    <path d="M15 6v14" />
  </Svg>
);

// Home / dashboard overview (for a future overview page).
export const IconHome = (p: IconProps) => (
  <Svg {...p}>
    <path d="M3 10.5 12 3l9 7.5" />
    <path d="M5 9.5V20a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1V9.5" />
    <path d="M9 21v-6h6v6" />
  </Svg>
);

/* --- content / actions ------------------------------------------------------- */

export const IconSparkles = (p: IconProps) => (
  <Svg {...p}>
    <path d="M9.94 14.06A2 2 0 0 0 8.5 12.6l-4.6-1.2a.5.5 0 0 1 0-.96l4.6-1.18A2 2 0 0 0 9.94 7.8l1.18-4.6a.5.5 0 0 1 .96 0l1.18 4.6a2 2 0 0 0 1.44 1.44l4.6 1.18a.5.5 0 0 1 0 .96l-4.6 1.18a2 2 0 0 0-1.44 1.46l-1.18 4.6a.5.5 0 0 1-.96 0Z" />
    <path d="M19 4v3" />
    <path d="M20.5 5.5h-3" />
    <path d="M5 17v2" />
    <path d="M6 18H4" />
  </Svg>
);

export const IconStar = ({ filled, ...p }: IconProps & { filled?: boolean }) => (
  <Svg {...p} fill={filled ? "currentColor" : "none"}>
    <path d="M11.52 3.3a.53.53 0 0 1 .96 0l2.1 4.26a.5.5 0 0 0 .4.28l4.7.68a.53.53 0 0 1 .3.9l-3.4 3.32a.5.5 0 0 0-.15.46l.8 4.68a.53.53 0 0 1-.77.56l-4.2-2.2a.5.5 0 0 0-.48 0l-4.2 2.2a.53.53 0 0 1-.77-.56l.8-4.68a.5.5 0 0 0-.15-.46l-3.4-3.32a.53.53 0 0 1 .3-.9l4.7-.68a.5.5 0 0 0 .4-.28Z" />
  </Svg>
);

export const IconPencil = (p: IconProps) => (
  <Svg {...p}>
    <path d="M21.17 6.81a1 1 0 0 0-3.98-3.99L3.84 16.17a2 2 0 0 0-.5.83l-1.32 4.35a.5.5 0 0 0 .62.62l4.35-1.32a2 2 0 0 0 .83-.5z" />
    <path d="m15 5 4 4" />
  </Svg>
);

export const IconPlay = (p: IconProps) => (
  <Svg {...p} fill="currentColor" stroke="none">
    <path d="M7 4.5a1 1 0 0 1 1.5-.87l11 7.5a1 1 0 0 1 0 1.74l-11 7.5A1 1 0 0 1 7 19.5Z" />
  </Svg>
);

export const IconDownload = (p: IconProps) => (
  <Svg {...p}>
    <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
    <path d="m7 10 5 5 5-5" />
    <path d="M12 15V3" />
  </Svg>
);

export const IconUpload = (p: IconProps) => (
  <Svg {...p}>
    <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
    <path d="m17 8-5-5-5 5" />
    <path d="M12 3v12" />
  </Svg>
);

export const IconCar = (p: IconProps) => (
  <Svg {...p}>
    <path d="M19 17h2a1 1 0 0 0 1-1v-3c0-.9-.7-1.7-1.5-1.9-1.8-.5-4.5-1.1-4.5-1.1s-1.3-1.4-2.2-2.3c-.5-.4-1.1-.7-1.8-.7H5c-.6 0-1.1.4-1.4.9l-1.4 2.9A3.7 3.7 0 0 0 2 12v4a1 1 0 0 0 1 1h2" />
    <circle cx="7" cy="17" r="2" />
    <path d="M9 17h6" />
    <circle cx="17" cy="17" r="2" />
  </Svg>
);

export const IconMic = (p: IconProps) => (
  <Svg {...p}>
    <path d="M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3Z" />
    <path d="M19 10v2a7 7 0 0 1-14 0v-2" />
    <line x1="12" y1="19" x2="12" y2="22" />
  </Svg>
);

// Detection zone / region of interest.
export const IconZone = (p: IconProps) => (
  <Svg {...p}>
    <rect x="3" y="3" width="18" height="18" rx="2" strokeDasharray="4 3" />
  </Svg>
);

export const IconExpand = (p: IconProps) => (
  <Svg {...p}>
    <path d="M8 3H5a2 2 0 0 0-2 2v3" />
    <path d="M21 8V5a2 2 0 0 0-2-2h-3" />
    <path d="M3 16v3a2 2 0 0 0 2 2h3" />
    <path d="M16 21h3a2 2 0 0 0 2-2v-3" />
  </Svg>
);

export const IconX = (p: IconProps) => (
  <Svg {...p}>
    <path d="M18 6 6 18" />
    <path d="m6 6 12 12" />
  </Svg>
);

/// Hash / tag mark (event tags).
export const IconTag = (p: IconProps) => (
  <Svg {...p}>
    <path d="M4 9h16" />
    <path d="M4 15h16" />
    <path d="M10 3 8 21" />
    <path d="M16 3l-2 18" />
  </Svg>
);

export const IconLink = (p: IconProps) => (
  <Svg {...p}>
    <path d="M10 13a5 5 0 0 0 7.07 0l3-3a5 5 0 1 0-7.07-7.07l-1.72 1.71" />
    <path d="M14 11a5 5 0 0 0-7.07 0l-3 3a5 5 0 1 0 7.07 7.07l1.71-1.71" />
  </Svg>
);

export const IconSearch = (p: IconProps) => (
  <Svg {...p}>
    <circle cx="11" cy="11" r="8" />
    <path d="m21 21-4.3-4.3" />
  </Svg>
);

export const IconCheck = (p: IconProps) => (
  <Svg {...p}>
    <path d="M20 6 9 17l-5-5" />
  </Svg>
);

export const IconSliders = (p: IconProps) => (
  <Svg {...p}>
    <line x1="21" x2="14" y1="4" y2="4" />
    <line x1="10" x2="3" y1="4" y2="4" />
    <line x1="21" x2="12" y1="12" y2="12" />
    <line x1="8" x2="3" y1="12" y2="12" />
    <line x1="21" x2="16" y1="20" y2="20" />
    <line x1="12" x2="3" y1="20" y2="20" />
    <line x1="14" x2="14" y1="2" y2="6" />
    <line x1="8" x2="8" y1="10" y2="14" />
    <line x1="16" x2="16" y1="18" y2="22" />
  </Svg>
);

export const IconCalendar = (p: IconProps) => (
  <Svg {...p}>
    <rect width="18" height="18" x="3" y="4" rx="2" />
    <path d="M3 9h18" />
    <path d="M8 2v4" />
    <path d="M16 2v4" />
  </Svg>
);

export const IconClock = (p: IconProps) => (
  <Svg {...p}>
    <circle cx="12" cy="12" r="9" />
    <path d="M12 7v5l3 2" />
  </Svg>
);

export const IconLayers = (p: IconProps) => (
  <Svg {...p}>
    <path d="m12 2 9 5-9 5-9-5 9-5Z" />
    <path d="m3 12 9 5 9-5" />
    <path d="m3 17 9 5 9-5" />
  </Svg>
);

/* 2x2 grid — the mobile "More" overflow tab. */
export const IconGrid = (p: IconProps) => (
  <Svg {...p}>
    <rect x="3" y="3" width="7" height="7" rx="1.5" />
    <rect x="14" y="3" width="7" height="7" rx="1.5" />
    <rect x="3" y="14" width="7" height="7" rx="1.5" />
    <rect x="14" y="14" width="7" height="7" rx="1.5" />
  </Svg>
);

/* --- chevrons / directional (selects, PTZ) ----------------------------------- */

export const IconChevronDown = (p: IconProps) => (
  <Svg {...p}>
    <path d="m6 9 6 6 6-6" />
  </Svg>
);
export const IconChevronUp = (p: IconProps) => (
  <Svg {...p}>
    <path d="m18 15-6-6-6 6" />
  </Svg>
);
export const IconChevronLeft = (p: IconProps) => (
  <Svg {...p}>
    <path d="m15 18-6-6 6-6" />
  </Svg>
);
export const IconChevronRight = (p: IconProps) => (
  <Svg {...p}>
    <path d="m9 18 6-6-6-6" />
  </Svg>
);

export const IconPlus = (p: IconProps) => (
  <Svg {...p}>
    <path d="M5 12h14" />
    <path d="M12 5v14" />
  </Svg>
);
export const IconMinus = (p: IconProps) => (
  <Svg {...p}>
    <path d="M5 12h14" />
  </Svg>
);

export const IconZoomIn = (p: IconProps) => (
  <Svg {...p}>
    <circle cx="11" cy="11" r="8" />
    <path d="m21 21-4.3-4.3" />
    <path d="M11 8v6" />
    <path d="M8 11h6" />
  </Svg>
);
export const IconZoomOut = (p: IconProps) => (
  <Svg {...p}>
    <circle cx="11" cy="11" r="8" />
    <path d="m21 21-4.3-4.3" />
    <path d="M8 11h6" />
  </Svg>
);

/* --- status / security / audit ---------------------------------------------- */

export const IconShield = (p: IconProps) => (
  <Svg {...p}>
    <path d="M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z" />
  </Svg>
);

export const IconLock = (p: IconProps) => (
  <Svg {...p}>
    <rect width="18" height="11" x="3" y="11" rx="2" />
    <path d="M7 11V7a5 5 0 0 1 10 0v4" />
  </Svg>
);

export const IconKey = (p: IconProps) => (
  <Svg {...p}>
    <circle cx="7.5" cy="15.5" r="4.5" />
    <path d="m10.7 12.3 9.3-9.3" />
    <path d="m16 6 3 3" />
    <path d="m13.5 8.5 2.5 2.5" />
  </Svg>
);

export const IconTicket = (p: IconProps) => (
  <Svg {...p}>
    <path d="M2 9a3 3 0 0 1 0 6v2a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-2a3 3 0 0 1 0-6V7a2 2 0 0 0-2-2H4a2 2 0 0 0-2 2Z" />
    <path d="M13 5v2" />
    <path d="M13 11v2" />
    <path d="M13 17v2" />
  </Svg>
);

export const IconTrash = (p: IconProps) => (
  <Svg {...p}>
    <path d="M3 6h18" />
    <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6" />
    <path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" />
    <line x1="10" x2="10" y1="11" y2="17" />
    <line x1="14" x2="14" y1="11" y2="17" />
  </Svg>
);

export const IconLogIn = (p: IconProps) => (
  <Svg {...p}>
    <path d="M15 3h4a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2h-4" />
    <polyline points="10 17 15 12 10 7" />
    <line x1="15" x2="3" y1="12" y2="12" />
  </Svg>
);

export const IconBan = (p: IconProps) => (
  <Svg {...p}>
    <circle cx="12" cy="12" r="9" />
    <path d="m5.6 5.6 12.8 12.8" />
  </Svg>
);

export const IconDatabase = (p: IconProps) => (
  <Svg {...p}>
    <ellipse cx="12" cy="5" rx="8" ry="3" />
    <path d="M4 5v14c0 1.66 3.58 3 8 3s8-1.34 8-3V5" />
    <path d="M4 12c0 1.66 3.58 3 8 3s8-1.34 8-3" />
  </Svg>
);

export const IconWifi = (p: IconProps) => (
  <Svg {...p}>
    <path d="M5 12.5a10 10 0 0 1 14 0" />
    <path d="M8.5 16a5 5 0 0 1 7 0" />
    <path d="M2 9a15 15 0 0 1 20 0" />
    <path d="M12 20h.01" />
  </Svg>
);

export const IconWifiOff = (p: IconProps) => (
  <Svg {...p}>
    <path d="M12 20h.01" />
    <path d="M8.5 16a5 5 0 0 1 6-.8" />
    <path d="M5 12.5a10 10 0 0 1 5.5-2.4" />
    <path d="M2 9a15 15 0 0 1 5.6-3.6" />
    <path d="M14 6.1A15 15 0 0 1 22 9" />
    <path d="m2 2 20 20" />
  </Svg>
);

// Generic alert/warning triangle (plate-of-interest, errors).
export const IconAlert = (p: IconProps) => (
  <Svg {...p}>
    <path d="M10.3 3.2 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.2a2 2 0 0 0-3.4 0Z" />
    <path d="M12 9v4" />
    <path d="M12 17h.01" />
  </Svg>
);

export const IconInfo = (p: IconProps) => (
  <Svg {...p}>
    <circle cx="12" cy="12" r="9" />
    <path d="M12 11v5" />
    <path d="M12 8h.01" />
  </Svg>
);

// Command palette / keyboard.
export const IconCommand = (p: IconProps) => (
  <Svg {...p}>
    <path d="M15 6a3 3 0 1 1 3 3H6a3 3 0 1 1 3-3v12a3 3 0 1 1-3-3h12a3 3 0 1 1-3 3Z" />
  </Svg>
);

/* --- PTZ directional arrows (full shaft + head, distinct from chevrons) ------ */

export const IconArrowUp = (p: IconProps) => (
  <Svg {...p}>
    <path d="M12 19V5" />
    <path d="m6 11 6-6 6 6" />
  </Svg>
);
export const IconArrowDown = (p: IconProps) => (
  <Svg {...p}>
    <path d="M12 5v14" />
    <path d="m6 13 6 6 6-6" />
  </Svg>
);
export const IconArrowLeft = (p: IconProps) => (
  <Svg {...p}>
    <path d="M19 12H5" />
    <path d="m11 6-6 6 6 6" />
  </Svg>
);
export const IconArrowRight = (p: IconProps) => (
  <Svg {...p}>
    <path d="M5 12h14" />
    <path d="m13 6 6 6-6 6" />
  </Svg>
);

/* --- transport stop + solid status/rec dots (tint via color at call site) ---- */

export const IconStop = (p: IconProps) => (
  <Svg {...p} fill="currentColor" stroke="none">
    <rect x="6" y="6" width="12" height="12" rx="2" />
  </Svg>
);
export const IconRecDot = (p: IconProps) => (
  <Svg {...p} fill="currentColor" stroke="none">
    <circle cx="12" cy="12" r="6" />
  </Svg>
);
export const IconStatusDot = (p: IconProps) => (
  <Svg {...p} fill="currentColor" stroke="none">
    <circle cx="12" cy="12" r="5" />
  </Svg>
);

/* --- snooze, retry, network-scan --------------------------------------------- */

export const IconMoon = (p: IconProps) => (
  <Svg {...p}>
    <path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z" />
  </Svg>
);
export const IconSun = (p: IconProps) => (
  <Svg {...p}>
    <circle cx="12" cy="12" r="4" />
    <path d="M12 2v2" />
    <path d="M12 20v2" />
    <path d="m4.9 4.9 1.4 1.4" />
    <path d="m17.7 17.7 1.4 1.4" />
    <path d="M2 12h2" />
    <path d="M20 12h2" />
    <path d="m6.3 17.7-1.4 1.4" />
    <path d="m19.1 4.9-1.4 1.4" />
  </Svg>
);
export const IconRefresh = (p: IconProps) => (
  <Svg {...p}>
    <path d="M3 12a9 9 0 0 1 15-6.7L21 8" />
    <path d="M21 3v5h-5" />
    <path d="M21 12a9 9 0 0 1-15 6.7L3 16" />
    <path d="M3 21v-5h5" />
  </Svg>
);
export const IconRadar = (p: IconProps) => (
  <Svg {...p}>
    <path d="M19.07 4.93A10 10 0 1 0 21 12" />
    <path d="M12 12 19 5" />
    <path d="M16.5 7.5a6 6 0 1 0 .9 1.2" />
    <circle cx="12" cy="12" r="1.2" fill="currentColor" stroke="none" />
  </Svg>
);

// Route / journey: two waypoints joined by a winding path (P3.1 journey fusion).
export const IconRoute = (p: IconProps) => (
  <Svg {...p}>
    <circle cx="6" cy="19" r="3" />
    <path d="M9 19h8.5a3.5 3.5 0 0 0 0-7h-11a3.5 3.5 0 0 1 0-7H15" />
    <circle cx="18" cy="5" r="3" />
  </Svg>
);

/* --- high-value hand poses (rest reuse IconHand + a text label) -------------- */

export const IconThumbUp = (p: IconProps) => (
  <Svg {...p}>
    <path d="M7 10v11" />
    <path d="M2 11a1 1 0 0 1 1-1h4v11H3a1 1 0 0 1-1-1Z" />
    <path d="M7 10.5 11 3a2.5 2.5 0 0 1 2.5 3l-.8 3.5H20a2 2 0 0 1 2 2.3l-1.1 6A2 2 0 0 1 18.9 21H7" />
  </Svg>
);
export const IconThumbDown = (p: IconProps) => (
  <Svg {...p}>
    <path d="M17 14V3" />
    <path d="M22 13a1 1 0 0 1-1 1h-4V3h4a1 1 0 0 1 1 1Z" />
    <path d="M17 13.5 13 21a2.5 2.5 0 0 1-2.5-3l.8-3.5H4a2 2 0 0 1-2-2.3l1.1-6A2 2 0 0 1 5.1 3H17" />
  </Svg>
);
export const IconVictory = (p: IconProps) => (
  <Svg {...p}>
    <path d="M9 11 7 4.5a1.5 1.5 0 0 1 2.9-.8L11.5 10" />
    <path d="M15 10.5 16.6 4a1.5 1.5 0 0 1 2.9.7L18 11.5" />
    <path d="M11.5 10v-.5a1.5 1.5 0 0 1 3 0v1.5l1 .5a3 3 0 0 1 1.5 2.6V17a4 4 0 0 1-4 4h-2a4 4 0 0 1-3.4-1.9L5 15.5a1.6 1.6 0 0 1 2.5-2L9 15V10" />
  </Svg>
);

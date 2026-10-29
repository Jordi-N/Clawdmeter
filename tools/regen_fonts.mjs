// Regenerates firmware/src/font_*.c with the Spanish glyphs (á é í ó ú ü ñ,
// lowercase) and applies the LVGL 9 patch from docs/fonts.md.
// Run from the repo root: node tools/regen_fonts.mjs
import { execFileSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';

const LATIN = '0x20-0x7E,0xE1,0xE9,0xED,0xF1,0xF3,0xFA,0xFC'; // lowercase only: capital accents and ¡¿ raise line_height and shift every layout
const MONO_EXTRA = ',0xB7,0x2026,0x2722,0x2733,0x2736,0x273B,0x273D,0x2713';

const fonts = [
  ...[56, 34].map((s) => ['tiempos', s, 'assets/TiemposText-400-Regular.otf', LATIN]),
  ...[48, 28, 24, 20, 16, 14, 12].map((s) => ['styrene', s, 'assets/StyreneB-Regular.otf', LATIN]),
  ...[32, 18].map((s) => ['mono', s, 'assets/DejaVuSansMono.ttf', LATIN + MONO_EXTRA]),
];

// lv_font_conv@1.5.3 emits the LVGL 8 layout; reshape it to the LVGL 9 one
// the checked-in fonts use (docs/fonts.md).
function patchLvgl9(src, name) {
  const guard = name.toUpperCase();
  return src
    .replace(/#ifdef LV_LVGL_H_INCLUDE_SIMPLE\n#include "lvgl.h"\n#else\n#include "lvgl.h"\n#endif\n/, '#include "lvgl.h"\n')
    .replace(`#ifndef ${guard}\n#define ${guard} 1\n#endif\n\n#if ${guard}\n`, '')
    .replace(`\n#endif /*#if ${guard}*/\n`, '\n')
    .replace(/#if LVGL_VERSION_MAJOR == 8\n\/\*Store all the custom data of the font\*\/\nstatic  lv_font_fmt_txt_glyph_cache_t cache;\n#endif\n/, '')
    .replace(/#if LVGL_VERSION_MAJOR >= 8\n(static const lv_font_fmt_txt_dsc_t font_dsc = \{)\n#else\n.*\n#endif\n/, '$1\n')
    .replace(/#if LVGL_VERSION_MAJOR == 8\n {4}\.cache = &cache\n#endif\n/, '')
    .replace(/#if LVGL_VERSION_MAJOR >= 8\n(const lv_font_t \w+ = \{)\n#else\n.*\n#endif\n/, '$1\n')
    .replace(/#if !\(LVGL_VERSION_MAJOR == 6 && LVGL_VERSION_MINOR == 0\)\n( {4}\.subpx = LV_FONT_SUBPX_NONE,)\n#endif\n#if LV_VERSION_CHECK\(7, 4, 0\) \|\| LVGL_VERSION_MAJOR >= 8\n/,
      '$1\n    .release_glyph = NULL,\n    .kerning = 0,\n    .static_bitmap = 0,\n')
    .replace(/( {4}\.underline_thickness = \d+,)\n#endif\n/, '$1\n')
    .replace(/#if LV_VERSION_CHECK\(8, 2, 0\) \|\| LVGL_VERSION_MAJOR >= 9\n( {4}\.fallback = NULL,)\n#endif\n/, '$1\n');
}

for (const [family, size, file, range] of fonts) {
  const name = `font_${family}_${size}`;
  const out = `firmware/src/${name}.c`;
  execFileSync('npx', ['-y', 'lv_font_conv@1.5.3', '--font', file, '-r', range, '--size', String(size),
    '--format', 'lvgl', '--bpp', '4', '--no-compress', '-o', out, '--lv-include', 'lvgl.h'],
    { stdio: 'inherit', shell: process.platform === 'win32' });
  const patched = patchLvgl9(readFileSync(out, 'utf8').replaceAll('\r\n', '\n'), name);
  if (/#if|#else|\.cache/.test(patched)) throw new Error(`${out}: LVGL 9 patch incomplete`);
  writeFileSync(out, patched);
  console.log(`${out} ok`);
}

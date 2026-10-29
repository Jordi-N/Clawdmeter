#pragma once

// Copy on the meters whose width decides the layout: ui.cpp draws these
// strings and layout.cpp sizes the screen for the widest of them, so a
// rewording here is checked by test_layout on every panel size.
// UTF-8: the fonts carry the Spanish glyphs (tools/regen_fonts.mjs).

// Pairing hint: heading, two steps, and a note line, per board kind / state
// (ui.cpp apply_pair_text).
static const char* const PAIR_TEXT_PWR[3]     = { "Emparejar", "pulsa encendido 3 s", "y luego suéltalo" };
static const char* const PAIR_TEXT_TOUCH[3]   = { "Emparejar", "mantén el dedo aquí", "durante 5 s" };
static const char* const PAIR_TEXT_CLEARED[3] = { "Listo", "elige Clawdmeter", "en tu ordenador" };
static const char* const PAIR_TEXT_HOLDING    = "sigue pulsando...";

// Card pills: session, weekly, then the spend-capped account's spending and
// period. The short set is for panels too narrow for the long one.
static const char* const PILL_TEXT[4]       = { "Sesión", "Semana", "Gasto", "Ciclo" };
static const char* const PILL_TEXT_SHORT[4] = { "5h", "7d", "Gasto", "Ciclo" };

// Widest text a card's reset line shows: the weekly window's last day, the
// spend-capped account's caption, and its period line (its widest pace word and date).
static const char* const RESET_TEXT_WIDEST[] = { "Reinicio 23h 59m", "Reinicio 6d 23h", "del límite de gasto" };
#define PERIOD_TEXT_WIDEST "Por debajo - Reinicio May 30"
#define PERIOD_TEXT_SHORT_WIDEST "Reinicio May 30"   // where no size fits the pace word too

// Status-line words every panel must fit (the whimsical verbs are skipped
// when they don't): "<spinner> <word>…".
static const char* const STATUS_TEXT_REQUIRED[] = {
    "\xE2\x9C\xBB Conectado\xE2\x80\xA6", "\xE2\x9C\xBB Atento\xE2\x80\xA6",
    "\xE2\x9C\xBB Esperando\xE2\x80\xA6", "\xE2\x9C\xBB Sin datos\xE2\x80\xA6",
    "\xE2\x9C\xBB En pausa\xE2\x80\xA6",
};

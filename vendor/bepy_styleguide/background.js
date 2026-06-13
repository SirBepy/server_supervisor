(function () {
  // ─── Config ───────────────────────────────────────────────────────────────
  // Disable:        set window.BEPY_BACKGROUND = false before this script
  // Custom pattern: set window.BEPY_BG_PATTERN = 'path/to/pattern.svg' before this script
  // Custom variants: set window.BEPY_BG_VARIANTS = [{ key, label, build }] before this script

  if (window.BEPY_BACKGROUND === false) return;

  var CDN =
    "https://cdn.jsdelivr.net/gh/sirbepy/sirbepy-styleguide@main/widget/";
  var patternUrl = window.BEPY_BG_PATTERN || CDN + "background_pattern.svg";
  var LS_BG_KEY = "tabs-labs-bg-variant";

  // ─── Variant registry ─────────────────────────────────────────────────────

  var VARIANTS = {
    pattern: { label: "Pattern", build: buildPattern },
    gradient: { label: "Gradient", build: buildGradient },
  };

  // Register per-project custom variants
  if (Array.isArray(window.BEPY_BG_VARIANTS)) {
    window.BEPY_BG_VARIANTS.forEach(function (v) {
      if (v.key && v.label && typeof v.build === "function") {
        VARIANTS[v.key] = { label: v.label, build: v.build };
      }
    });
  }

  var DEFAULT_VARIANT = "pattern";

  // ─── Shared container ─────────────────────────────────────────────────────

  var bg = document.createElement("div");
  bg.id = "bepy-bg";

  var sharedStyle = document.createElement("style");
  sharedStyle.id = "bepy-bg-style";
  sharedStyle.textContent = `
    html, body { background: transparent !important; }

    #bepy-bg {
      position: fixed;
      inset: 0;
      z-index: -1;
      overflow: hidden;
      pointer-events: none;
    }
  `;

  // ─── Current state ────────────────────────────────────────────────────────

  var _currentVariant = null;
  var _variantStyle = null;

  // ─── Public API ───────────────────────────────────────────────────────────

  function getActiveVariant() {
    return localStorage.getItem(LS_BG_KEY) || DEFAULT_VARIANT;
  }

  function setVariant(name) {
    if (!VARIANTS[name]) return;
    localStorage.setItem(LS_BG_KEY, name);
    _applyVariant(name);
  }

  function getVariants() {
    return Object.keys(VARIANTS).map(function (key) {
      return { key: key, label: VARIANTS[key].label };
    });
  }

  // Expose for settings panel
  window.BEPY_BG = {
    getVariants: getVariants,
    getActive: getActiveVariant,
    set: setVariant,
  };

  // ─── Internal ─────────────────────────────────────────────────────────────

  function _applyVariant(name) {
    if (_currentVariant === name) return;
    _currentVariant = name;

    // Clear previous content
    bg.innerHTML = "";
    if (_variantStyle) _variantStyle.remove();

    // Build new variant
    var result = VARIANTS[name].build();
    _variantStyle = document.createElement("style");
    _variantStyle.id = "bepy-bg-variant-style";
    _variantStyle.textContent = result.css;
    document.head.appendChild(_variantStyle);

    bg.appendChild(result.el);
  }

  // ─── Variant: Pattern (original) ──────────────────────────────────────────

  function buildPattern() {
    var fill = document.createElement("div");
    fill.id = "bepy-bg-fill";
    var pattern = document.createElement("div");
    pattern.id = "bepy-bg-pattern";
    fill.appendChild(pattern);

    var css = `
      #bepy-bg-fill {
        position: relative;
        width: 100%;
        height: 100%;
        background: radial-gradient(
          ellipse at 50% 60%,
          var(--color-background) 0%,
          var(--color-background) 200%
        );
      }

      #bepy-bg-pattern {
        position: absolute;
        inset: 0;
        opacity: 0.08;
        -webkit-mask-image: radial-gradient(ellipse at 50% 60%, black 40%, transparent 80%);
        mask-image: radial-gradient(ellipse at 50% 60%, black 40%, transparent 80%);
      }

      #bepy-bg-pattern::before {
        content: '';
        position: absolute;
        inset: -200px;
        background: var(--color-primary, #9d7dfc);
        -webkit-mask-image: url("${patternUrl}");
        mask-image: url("${patternUrl}");
        -webkit-mask-size: 120px 120px;
        mask-size: 120px 120px;
        animation: bepy-pan 30s linear infinite;
        will-change: transform;
      }

      @keyframes bepy-pan {
        0%   { transform: translate(0, 0); }
        100% { transform: translate(120px, -240px); }
      }
    `;

    return { el: fill, css: css };
  }

  // ─── Variant: Gradient ────────────────────────────────────────────────────

  function buildGradient() {
    var wrap = document.createElement("div");
    wrap.id = "bepy-bg-gradient";

    for (var i = 1; i <= 4; i++) {
      var blob = document.createElement("div");
      blob.className = "bepy-grad-blob bepy-grad-blob-" + i;
      wrap.appendChild(blob);
    }

    var css = `
      #bepy-bg-gradient {
        position: relative;
        width: 100%;
        height: 100%;
        background: var(--color-background, #16151f);
        overflow: hidden;
      }

      .bepy-grad-blob {
        position: absolute;
        border-radius: 50%;
        filter: blur(130px) brightness(0.45);
        will-change: transform;
        mix-blend-mode: screen;
      }

      [data-mode="light"] .bepy-grad-blob {
        mix-blend-mode: multiply;
        filter: blur(150px) brightness(1);
      }

      .bepy-grad-blob-1 {
        width: 70vmax;
        height: 70vmax;
        background: var(--color-primary, #9d7dfc);
        top: -15%;
        left: -15%;
        opacity: 0.5;
        animation: bepy-grad-1 26s ease-in-out infinite;
      }

      .bepy-grad-blob-2 {
        width: 65vmax;
        height: 65vmax;
        background: var(--color-secondary, #6e8fff);
        bottom: -15%;
        right: -15%;
        opacity: 0.45;
        animation: bepy-grad-2 30s ease-in-out infinite;
      }

      .bepy-grad-blob-3 {
        width: 55vmax;
        height: 55vmax;
        background: var(--color-accent, #9d7dfc);
        top: 20%;
        left: 20%;
        opacity: 0.35;
        animation: bepy-grad-3 34s ease-in-out infinite;
      }

      .bepy-grad-blob-4 {
        width: 50vmax;
        height: 50vmax;
        background: var(--color-info, #6e8fff);
        top: 10%;
        right: 15%;
        opacity: 0.3;
        animation: bepy-grad-4 28s ease-in-out infinite;
      }

      [data-mode="light"] .bepy-grad-blob-1 { opacity: 0.4; }
      [data-mode="light"] .bepy-grad-blob-2 { opacity: 0.35; }
      [data-mode="light"] .bepy-grad-blob-3 { opacity: 0.28; }
      [data-mode="light"] .bepy-grad-blob-4 { opacity: 0.22; }

      @keyframes bepy-grad-1 {
        0%   { transform: translate(0, 0) scale(1); }
        25%  { transform: translate(40vw, 30vh) scale(1.1); }
        50%  { transform: translate(60vw, 60vh) scale(0.95); }
        75%  { transform: translate(20vw, 70vh) scale(1.05); }
        100% { transform: translate(0, 0) scale(1); }
      }

      @keyframes bepy-grad-2 {
        0%   { transform: translate(0, 0) scale(1); }
        25%  { transform: translate(-50vw, -25vh) scale(1.05); }
        50%  { transform: translate(-60vw, -55vh) scale(1.1); }
        75%  { transform: translate(-20vw, -65vh) scale(0.9); }
        100% { transform: translate(0, 0) scale(1); }
      }

      @keyframes bepy-grad-3 {
        0%   { transform: translate(0, 0) scale(1); }
        25%  { transform: translate(30vw, -35vh) scale(1.15); }
        50%  { transform: translate(-25vw, -20vh) scale(0.85); }
        75%  { transform: translate(-40vw, 30vh) scale(1.1); }
        100% { transform: translate(0, 0) scale(1); }
      }

      @keyframes bepy-grad-4 {
        0%   { transform: translate(0, 0) scale(1); }
        25%  { transform: translate(-30vw, 40vh) scale(0.9); }
        50%  { transform: translate(20vw, 50vh) scale(1.15); }
        75%  { transform: translate(45vw, -10vh) scale(1.05); }
        100% { transform: translate(0, 0) scale(1); }
      }
    `;

    return { el: wrap, css: css };
  }

  // ─── Mount ────────────────────────────────────────────────────────────────

  function mount() {
    document.head.appendChild(sharedStyle);
    document.body.insertBefore(bg, document.body.firstChild);
    _applyVariant(getActiveVariant());
  }

  if (document.body) {
    mount();
  } else {
    document.addEventListener("DOMContentLoaded", mount);
  }
})();

/*
 * Capture Button site — small progressive enhancements only.
 * Links and content work without this file; it adds the lightbox, copy
 * buttons, scroll state, pointer glow, and the local release preview.
 */
(function () {
  "use strict";

  var reduceQuery = window.matchMedia("(prefers-reduced-motion: reduce)");
  var langCode = (document.documentElement.lang || "en").toLowerCase().slice(0, 2);
  var strings = {
    ru: {
      view: "Просмотр скриншотов",
      close: "Закрыть",
      prev: "Предыдущий скриншот",
      next: "Следующий скриншот",
      copied: "SHA-256 скопирован в буфер обмена.",
      manual: "Буфер обмена недоступен — хэш выделен, скопируйте его вручную.",
    },
    en: {
      view: "Screenshot viewer",
      close: "Close",
      prev: "Previous screenshot",
      next: "Next screenshot",
      copied: "SHA-256 copied to the clipboard.",
      manual: "Clipboard is unavailable — the hash is selected; copy it manually.",
    },
    zh: {
      view: "截图查看器",
      close: "关闭",
      prev: "上一张截图",
      next: "下一张截图",
      copied: "SHA-256 已复制到剪贴板。",
      manual: "剪贴板不可用 — 已选中哈希，请手动复制。",
    },
  };
  var t = strings[langCode] || strings.en;

  /* Current year in the footer. */
  document.querySelectorAll("[data-year]").forEach(function (node) {
    node.textContent = String(new Date().getFullYear());
  });

  /* Remember the chosen language when a visitor switches pages. */
  document.querySelectorAll("[data-lang-set]").forEach(function (link) {
    link.addEventListener("click", function () {
      try {
        localStorage.setItem("capture-button-lang", link.dataset.langSet);
      } catch (error) {
        /* Private mode can reject storage; the link still opens the page. */
      }
    });
  });

  /* Compact sticky header + active section in the main nav. */
  var header = document.querySelector("[data-header]");
  var sections = Array.prototype.slice.call(document.querySelectorAll("main section[id]"));
  var navLinks = Array.prototype.slice.call(document.querySelectorAll(".main-nav a"));
  var scrollQueued = false;

  function updateScrollState() {
    scrollQueued = false;
    if (header) header.classList.toggle("is-scrolled", window.scrollY > 16);

    var marker = window.scrollY + window.innerHeight * 0.33;
    var active = "";
    sections.forEach(function (section) {
      var top = section.getBoundingClientRect().top + window.scrollY;
      if (top <= marker) active = section.id;
    });

    navLinks.forEach(function (link) {
      if (active && link.getAttribute("href") === "#" + active) {
        link.setAttribute("aria-current", "location");
      } else {
        link.removeAttribute("aria-current");
      }
    });
  }

  function queueScrollState() {
    if (scrollQueued) return;
    scrollQueued = true;
    window.requestAnimationFrame(updateScrollState);
  }

  window.addEventListener("scroll", queueScrollState, { passive: true });
  window.addEventListener("resize", queueScrollState, { passive: true });
  updateScrollState();

  /* Copy SHA-256 values, with a selected-text fallback when clipboard access fails. */
  var copyStatus = document.createElement("p");
  copyStatus.className = "sr-only";
  copyStatus.setAttribute("role", "status");
  copyStatus.setAttribute("aria-live", "polite");
  document.body.appendChild(copyStatus);

  function checkIcon() {
    return '<svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 12.5 9.5 18 20 6.5"/></svg>';
  }

  document.querySelectorAll(".copy").forEach(function (button) {
    button.addEventListener("click", async function () {
      var key = button.dataset.copy;
      var code = document.querySelector("[data-hash-" + key + "]");
      if (!code) return;
      var value = code.textContent.trim();
      if (!value) return;
      var originalIcon = button.innerHTML;
      var originalLabel = button.getAttribute("aria-label") || "";

      try {
        await navigator.clipboard.writeText(value);
        copyStatus.textContent = t.copied;
        button.innerHTML = checkIcon();
        button.classList.add("is-copied");
        button.setAttribute("aria-label", t.copied);
        window.setTimeout(function () {
          button.innerHTML = originalIcon;
          button.classList.remove("is-copied");
          button.setAttribute("aria-label", originalLabel);
        }, 1600);
      } catch (error) {
        var range = document.createRange();
        range.selectNodeContents(code);
        var selection = window.getSelection();
        if (selection) {
          selection.removeAllRanges();
          selection.addRange(range);
        }
        copyStatus.textContent = t.manual;
      }
    });
  });

  /* Soft pointer highlight on large cards (fine pointers only). */
  if (!reduceQuery.matches && window.matchMedia("(pointer: fine)").matches) {
    document.querySelectorAll(".feature, .format-card").forEach(function (card) {
      card.addEventListener("pointermove", function (event) {
        var rect = card.getBoundingClientRect();
        card.style.setProperty("--mx", event.clientX - rect.left + "px");
        card.style.setProperty("--my", event.clientY - rect.top + "px");
      });
    });
  }

  /* Reveal content below the first viewport as it scrolls into view. */
  if ("IntersectionObserver" in window && !reduceQuery.matches) {
    var revealObserver = new IntersectionObserver(
      function (entries) {
        entries.forEach(function (entry) {
          if (!entry.isIntersecting) return;
          entry.target.classList.add("reveal-in");
          entry.target.classList.remove("reveal-pending");
          revealObserver.unobserve(entry.target);
        });
      },
      { rootMargin: "0px 0px -8% 0px", threshold: 0.08 }
    );

    document.querySelectorAll(".reveal").forEach(function (element) {
      var rect = element.getBoundingClientRect();
      if (rect.top > window.innerHeight * 0.92) {
        element.classList.add("reveal-pending");
        revealObserver.observe(element);
      }
    });
  }

  /* Screenshot lightbox. Every trigger is a real button, so this works by keyboard. */
  var triggers = Array.prototype.slice.call(
    document.querySelectorAll(".shot-btn, .frame-btn")
  );
  if (triggers.length) {
    var dialog = document.createElement("dialog");
    dialog.className = "lightbox";
    dialog.setAttribute("aria-label", t.view);
    dialog.innerHTML =
      '<button class="lightbox-close" type="button" aria-label="' + t.close + '">' +
      '<svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" aria-hidden="true"><path d="M6 6l12 12M18 6 6 18"/></svg>' +
      "</button>" +
      '<button class="lightbox-nav lightbox-prev" type="button" aria-label="' + t.prev + '">' +
      '<svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m14.5 6-6 6 6 6"/></svg>' +
      "</button>" +
      '<figure class="lightbox-frame">' +
      '<img alt="">' +
      "<figcaption></figcaption>" +
      '<span class="lightbox-count" aria-live="polite"></span>' +
      "</figure>" +
      '<button class="lightbox-nav lightbox-next" type="button" aria-label="' + t.next + '">' +
      '<svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m9.5 6 6 6-6 6"/></svg>' +
      "</button>";

    document.body.appendChild(dialog);

    var viewImage = dialog.querySelector(".lightbox-frame img");
    var caption = dialog.querySelector("figcaption");
    var counter = dialog.querySelector(".lightbox-count");
    var closeButton = dialog.querySelector(".lightbox-close");
    var index = 0;
    var lastTrigger = null;
    var closing = false;

    function show(next) {
      index = (next + triggers.length) % triggers.length;
      var trigger = triggers[index];
      var image = trigger.querySelector("img");
      if (!image) {
        viewImage.removeAttribute("src");
        viewImage.alt = "";
        caption.textContent = "";
        return;
      }
      viewImage.src = image.currentSrc || image.src;
      viewImage.alt = image.alt;
      var figcaption = trigger.closest("figure")
        ? trigger.closest("figure").querySelector(".cap-text")
        : null;
      caption.textContent = figcaption ? figcaption.textContent.trim() : image.alt;
      counter.textContent = index + 1 + " / " + triggers.length;
    }

    function open(start, trigger) {
      closing = false;
      lastTrigger = trigger || null;
      dialog.classList.remove("is-closing");
      show(start);
      if (!dialog.open) dialog.showModal();
      closeButton.focus();
    }

    function close() {
      if (!dialog.open || closing) return;
      if (reduceQuery.matches) {
        dialog.close();
        return;
      }
      closing = true;
      dialog.classList.add("is-closing");
      var frame = dialog.querySelector(".lightbox-frame");
      frame.addEventListener(
        "animationend",
        function () {
          dialog.classList.remove("is-closing");
          closing = false;
          if (dialog.open) dialog.close();
        },
        { once: true }
      );
    }

    triggers.forEach(function (trigger, i) {
      trigger.addEventListener("click", function () {
        open(i, trigger);
      });
    });

    closeButton.addEventListener("click", close);
    dialog.querySelector(".lightbox-prev").addEventListener("click", function () {
      show(index - 1);
    });
    dialog.querySelector(".lightbox-next").addEventListener("click", function () {
      show(index + 1);
    });
    dialog.addEventListener("click", function (event) {
      if (event.target === dialog) close();
    });
    dialog.addEventListener("cancel", function (event) {
      event.preventDefault();
      close();
    });
    dialog.addEventListener("keydown", function (event) {
      if (event.key === "ArrowLeft") {
        event.preventDefault();
        show(index - 1);
      } else if (event.key === "ArrowRight") {
        event.preventDefault();
        show(index + 1);
      } else if (event.key === "Home") {
        event.preventDefault();
        show(0);
      } else if (event.key === "End") {
        event.preventDefault();
        show(triggers.length - 1);
      }
    });
    dialog.addEventListener("close", function () {
      if (lastTrigger) lastTrigger.focus();
    });
  }

  /*
   * Local preview only: CI replaces @@VERSION@@, hashes and sizes in the HTML.
   * When those placeholders are still present, ask the GitHub release API and
   * fall back to the releases page if the request fails or no release exists.
   */
  var TOKEN = "@@";
  var tokenNodes = Array.prototype.slice.call(
    document.querySelectorAll(
      "[data-version], [data-hash-setup], [data-hash-zip], [data-size-setup], [data-size-zip], [data-dl-setup], [data-dl-zip]"
    )
  );
  var hasTokens = tokenNodes.some(function (node) {
    return (
      node.textContent.indexOf(TOKEN) !== -1 ||
      (node.getAttribute("href") || "").indexOf(TOKEN) !== -1
    );
  });

  var RELEASES_PAGE = "https://github.com/machine202502/capture-button/releases/latest";

  function setFallbackLinks() {
    document.querySelectorAll("[data-dl-setup], [data-dl-zip]").forEach(function (link) {
      link.href = RELEASES_PAGE;
    });
  }

  function useFallbackLabels() {
    document.querySelectorAll("[data-dl-setup], [data-dl-zip]").forEach(function (link) {
      if (link.dataset.fallback) link.textContent = link.dataset.fallback;
    });
  }

  function hideRemainingTokens() {
    tokenNodes.forEach(function (node) {
      var isLink = node.hasAttribute("href");
      var filled = isLink
        ? (node.getAttribute("href") || "").indexOf(TOKEN) === -1
        : node.textContent.indexOf(TOKEN) === -1;
      if (filled) return;
      var row = node.closest(".meta > div");
      if (row) {
        row.style.display = "none";
      } else if (node.classList.contains("status-pill")) {
        node.style.display = "none";
      }
    });
  }

  function setFileName(link, fallbackName) {
    if (!link) return;
    if (link.dataset.fallback) link.textContent = link.dataset.fallback;
    else link.textContent = fallbackName;
  }

  function fillRelease(release) {
    var assets = (release && release.assets) || [];
    var version = String((release && release.tag_name) || "").replace(/^v/, "");
    var setup = assets.find(function (asset) {
      return /-setup\.exe$/i.test(asset.name);
    });
    var zip = assets.find(function (asset) {
      return /\.zip$/i.test(asset.name) && !/-setup\.exe$/i.test(asset.name);
    });

    if (!version) {
      setFallbackLinks();
      useFallbackLabels();
      hideRemainingTokens();
      return;
    }

    document.querySelectorAll("[data-version]").forEach(function (node) {
      if (node.closest("a")) node.textContent = "-" + version + "-";
      else node.textContent = node.textContent.split("@@VERSION@@").join(version);
    });

    if (setup) {
      document.querySelectorAll("[data-dl-setup]").forEach(function (link) {
        link.href = setup.browser_download_url;
      });
    } else {
      document.querySelectorAll("[data-dl-setup]").forEach(function (link) {
        setFileName(link, "CaptureButton-windows-x64-setup.exe");
      });
    }

    if (zip) {
      document.querySelectorAll("[data-dl-zip]").forEach(function (link) {
        link.href = zip.browser_download_url;
      });
    } else {
      document.querySelectorAll("[data-dl-zip]").forEach(function (link) {
        setFileName(link, "CaptureButton-windows-x64.zip");
      });
    }

    var unit = document.documentElement.dataset.sizeUnit || "MB";
    [
      [setup, "setup"],
      [zip, "zip"],
    ].forEach(function (pair) {
      var asset = pair[0];
      var key = pair[1];
      var sizeNode = document.querySelector("[data-size-" + key + "]");
      var hashNode = document.querySelector("[data-hash-" + key + "]");

      if (sizeNode && sizeNode.textContent.indexOf(TOKEN) !== -1) {
        sizeNode.textContent = asset
          ? (asset.size / 1048576).toFixed(1) + " " + unit
          : "\u2014";
      }

      if (hashNode && hashNode.textContent.indexOf(TOKEN) !== -1) {
        var digest = asset && typeof asset.digest === "string" ? asset.digest : "";
        var hash = digest.indexOf("sha256:") === 0 ? digest.slice(7) : "";
        if (hash) {
          hashNode.textContent = hash;
        } else {
          var row = hashNode.closest(".meta > div");
          if (row) row.style.display = "none";
        }
      }
    });

    hideRemainingTokens();
  }

  if (hasTokens) {
    setFallbackLinks();
    window
      .fetch("https://api.github.com/repos/machine202502/capture-button/releases/latest", {
        headers: { Accept: "application/vnd.github+json" },
      })
      .then(function (response) {
        return response.ok ? response.json() : null;
      })
      .then(function (release) {
        if (release) {
          fillRelease(release);
        } else {
          useFallbackLabels();
          hideRemainingTokens();
        }
      })
      .catch(function () {
        useFallbackLabels();
        hideRemainingTokens();
      });
  }
})();

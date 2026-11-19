(function () {
  "use strict";

  var prefersReduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  var shaderMix = 0;

  function getWebGLContext(canvas, options) {
    try {
      return canvas.getContext("webgl", options) || canvas.getContext("experimental-webgl", options);
    } catch (e) {
      return null;
    }
  }

  function createProgramAsync(gl, vertexSource, fragmentSource, onReady) {
    function compile(type, source) {
      var shader = gl.createShader(type);
      gl.shaderSource(shader, source);
      gl.compileShader(shader);
      return shader;
    }

    var vertex = compile(gl.VERTEX_SHADER, vertexSource);
    var fragment = compile(gl.FRAGMENT_SHADER, fragmentSource);
    var program = gl.createProgram();
    gl.attachShader(program, vertex);
    gl.attachShader(program, fragment);
    gl.linkProgram(program);
    var parallel = gl.getExtension("KHR_parallel_shader_compile");

    function check() {
      if (parallel && !gl.getProgramParameter(program, parallel.COMPLETION_STATUS_KHR)) {
        requestAnimationFrame(check);
        return;
      }
      var ready = gl.getProgramParameter(program, gl.LINK_STATUS);
      gl.deleteShader(vertex);
      gl.deleteShader(fragment);
      if (!ready) gl.deleteProgram(program);
      onReady(ready ? program : null);
    }

    if (parallel) requestAnimationFrame(check);
    else check();
  }

  function preferredDpr(maxDpr) {
    var cores = navigator.hardwareConcurrency || 8;
    var memory = navigator.deviceMemory || 8;
    var mobile = window.matchMedia("(max-width: 720px)").matches;
    var cap = mobile || cores <= 4 || memory <= 4 ? Math.min(maxDpr, 1.1) : maxDpr;
    return Math.max(1, Math.min(window.devicePixelRatio || 1, cap));
  }

  function scheduleIdle(callback, timeout) {
    if (typeof window.requestIdleCallback === "function") {
      window.requestIdleCallback(callback, { timeout: timeout || 1200 });
    } else {
      window.setTimeout(callback, 80);
    }
  }

  function wobbleHtml(text) {
    var units = [];
    var punctuation = ".!?…";
    for (var i = 0; i < text.length; i++) {
      var ch = text[i];
      if (punctuation.indexOf(ch) !== -1 && units.length) {
        units[units.length - 1] += ch;
      } else {
        units.push(ch);
      }
    }
    var html = "";
    units.forEach(function (unit, index) {
      html += '<span class="wobble-letter" style="--i:' + index + '">' + unit.replace(/ /g, "&nbsp;") + "</span>";
    });
    return html;
  }

  function applyWobble(el) {
    if (prefersReduced) {
      el.textContent = el.textContent;
      el.classList.remove("is-wobble");
      return;
    }
    el.innerHTML = wobbleHtml(el.textContent);
    el.classList.add("is-wobble");
  }

  function scrambleTo(el, finalText) {
    el.classList.remove("is-wobble");
    if (!finalText || prefersReduced) {
      el.textContent = finalText;
      return;
    }
    var chars = "ABCDEFGHIJKLMNOPQRSTUVWXYZАБВГДЕЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯ0123456789";
    var frame = 0;
    var total = 28;
    var tick = function () {
      frame++;
      var progress = frame / total;
      var out = "";
      for (var i = 0; i < finalText.length; i++) {
        var c = finalText[i];
        if (c === " " || c === ".") {
          out += c;
        } else if (i / finalText.length < progress) {
          out += c;
        } else {
          out += chars[Math.floor(Math.random() * chars.length)];
        }
      }
      el.textContent = out;
      if (frame < total) {
        requestAnimationFrame(tick);
      } else {
        el.innerHTML = wobbleHtml(finalText);
        el.classList.add("is-wobble");
      }
    };
    requestAnimationFrame(tick);
  }

  function initHeroIntro() {
    document.querySelectorAll(".hero-intro").forEach(function (el) {
      var delay = parseInt(el.getAttribute("data-delay") || "0", 10);
      el.style.setProperty("--delay", delay);
    });
    var title = document.querySelector(".hero-title [data-scramble]");
    if (!title || prefersReduced) return;
    window.setTimeout(function () {
      scrambleTo(title, title.textContent);
    }, 180);
  }

  /* ---------- WebGL aurora background ---------- */
  function initShader() {
    var canvas = document.getElementById("shader");
    if (!canvas) return;

    var gl = getWebGLContext(canvas, { alpha: false, antialias: false, powerPreference: "high-performance" });
    if (!gl) return;

    // The nebula keeps the original shader, same math, full resolution, drawn
    // every frame. The stars are no longer evaluated for every screen pixel:
    // they are ~1000 additive points, so their cost is measured in vertices.
    var STAR_DRIFT = 0;       // no drift: the stars only twinkle and parallax

    var vertex = [
      "attribute vec2 a_pos;",
      "varying vec2 v_uv;",
      "void main(){ v_uv = a_pos * 0.5 + 0.5; gl_Position = vec4(a_pos, 0.0, 1.0); }"
    ].join("\n");

    var nebulaFragment = [
      "precision highp float;",
      "uniform vec2 u_res;",
      "uniform float u_time;",
      "uniform float u_mix;",
      "uniform float u_scroll;",
      "float hash(vec2 p){ return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453123); }",
      "float noise(vec2 p){",
      "  vec2 i = floor(p); vec2 f = fract(p);",
      "  f = f * f * (3.0 - 2.0 * f);",
      "  return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), f.x),",
      "             mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), f.x), f.y);",
      "}",
      "float fbm(vec2 p){",
      "  float v = 0.0; float a = 0.5;",
      "  for (int i = 0; i < 5; i++){ v += a * noise(p); p *= 2.02; a *= 0.5; }",
      "  return v;",
      "}",
      "void main(){",
      "  vec2 uv = gl_FragCoord.xy / u_res.xy;",
      // the cloud drifts with the page: only the noise coordinate moves, the
      // side and vertical masks stay pinned to the screen
      "  vec2 p = (gl_FragCoord.xy - 0.5 * u_res.xy) / u_res.y;",
      "  p.y -= u_scroll * 0.05;",
      "  float t = u_time * 0.06;",
      "  vec2 q = vec2(fbm(p + vec2(0.0, 0.0) + t), fbm(p + vec2(5.2, 1.3) - t));",
      "  vec2 r = vec2(fbm(p + 2.0 * q + vec2(1.7, 9.2) + 0.15 * t), fbm(p + 2.0 * q + vec2(8.3, 2.8) - 0.12 * t));",
      "  float f = fbm(p + 1.9 * r);",
      "  vec3 deep = mix(vec3(0.02, 0.06, 0.20), vec3(0.12, 0.04, 0.24), u_mix);",
      "  vec3 blue = mix(vec3(0.10, 0.35, 0.72), vec3(0.46, 0.18, 0.84), u_mix);",
      "  vec3 cyan = mix(vec3(0.08, 0.70, 0.80), vec3(0.68, 0.42, 0.96), u_mix);",
      "  vec3 col = mix(deep, blue, smoothstep(0.0, 0.7, f));",
      "  col = mix(col, cyan, smoothstep(0.45, 1.0, f) * 0.7);",
      "  float side = smoothstep(0.28, 0.95, abs(uv.x - 0.5) * 2.0);",
      "  col *= mix(0.07, 1.0, side);",
      "  col += (1.0 - side) * vec3(0.16, 0.30, 0.46) * 0.07;",
      "  float vertical = smoothstep(0.0, 0.12, uv.y) * smoothstep(0.0, 0.12, 1.0 - uv.y);",
      "  col *= mix(0.55, 1.0, vertical);",
      "  gl_FragColor = vec4(col, 1.0);",
      "}"
    ].join("\n");

    // Star attributes: x, y, seed, parallax speed | r, g, b, kind | size.
    var starVertex = [
      "attribute vec4 a_a;",
      "attribute vec4 a_c;",
      "attribute float a_s;",
      "uniform vec2 u_res;",
      "uniform float u_time;",
      "uniform float u_scroll;",
      "uniform float u_drift;",
      "varying vec3 v_col;",
      "varying float v_mask;",
      "varying float v_tw;",
      "float hash(vec2 p){ return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453123); }",
      "float noise(vec2 p){",
      "  vec2 i = floor(p); vec2 f = fract(p);",
      "  f = f * f * (3.0 - 2.0 * f);",
      "  return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), f.x),",
      "             mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), f.x), f.y);",
      "}",
      "float fbm(vec2 p){",
      "  float v = 0.0; float a = 0.5;",
      "  for (int i = 0; i < 5; i++){ v += a * noise(p); p *= 2.02; a *= 0.5; }",
      "  return v;",
      "}",
      "void main(){",
      "  float y = fract(a_a.y + u_time * u_drift + u_scroll * a_a.w);",
      "  gl_Position = vec4(vec2(a_a.x, y) * 2.0 - 1.0, 0.0, 1.0);",
      "  gl_PointSize = a_s * u_res.y;",
      // The fog term is the same nebula field, evaluated once per star instead of
      // once per pixel; it keeps the original star dimming inside the cloud.
      "  vec2 frag = vec2(a_a.x * u_res.x, y * u_res.y);",
      "  vec2 p = (frag - 0.5 * u_res) / u_res.y;",
      "  p.y -= u_scroll * 0.05;",
      "  float t = u_time * 0.06;",
      "  vec2 q = vec2(fbm(p + t), fbm(p + vec2(5.2, 1.3) - t));",
      "  vec2 r = vec2(fbm(p + 2.0 * q + vec2(1.7, 9.2) + 0.15 * t), fbm(p + 2.0 * q + vec2(8.3, 2.8) - 0.12 * t));",
      "  float f = fbm(p + 1.9 * r);",
      "  float fog = smoothstep(0.25, 0.7, f);",
      "  float side = smoothstep(0.28, 0.95, abs(a_a.x - 0.5) * 2.0);",
      "  float mask = a_c.a < 0.5 ? mix(0.9, 1.0, side) : (1.0 - side);",
      "  mask *= a_c.a < 0.5 ? (0.6 + 0.4 * fog) : (0.45 + 0.55 * fog);",
      "  v_mask = mask;",
      "  v_tw = 0.65 + 0.35 * sin(u_time * (0.7 + a_a.z * 1.8) + a_a.z * 6.28318);",
      "  v_col = a_c.rgb;",
      "}"
    ].join("\n");

    var starFragment = [
      "precision mediump float;",
      "varying vec3 v_col;",
      "varying float v_mask;",
      "varying float v_tw;",
      "void main(){",
      "  if (v_mask < 0.01) discard;",
      "  float d = length(gl_PointCoord - 0.5) * 2.0;",
      "  float s = (1.0 - smoothstep(0.0, 0.34, d)) + (1.0 - smoothstep(0.0, 1.0, d)) * 0.22;",
      "  gl_FragColor = vec4(v_col * s * v_tw * v_mask, 1.0);",
      "}"
    ].join("\n");

    var grainFragment = [
      "precision mediump float;",
      "uniform vec2 u_res;",
      "uniform float u_time;",
      "float hash(vec2 p){ return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453123); }",
      "void main(){",
      "  vec2 uv = gl_FragCoord.xy / u_res.xy;",
      "  float side = smoothstep(0.28, 0.95, abs(uv.x - 0.5) * 2.0);",
      "  float n = hash(gl_FragCoord.xy + u_time) - 0.5;",
      "  gl_FragColor = vec4(vec3(n * mix(0.004, 0.02, side)), 1.0);",
      "}"
    ].join("\n");

    var blitFragment = [
      "precision mediump float;",
      "uniform sampler2D u_neb;",
      "varying vec2 v_uv;",
      "void main(){ gl_FragColor = vec4(texture2D(u_neb, v_uv).rgb, 1.0); }"
    ].join("\n");

    // Star counts follow the density of the procedural layers: the cell grid of
    // each layer times its threshold. Same colours, speeds and masks.
    function makeStars() {
      var layers = [
        { n: 60, scale: 16, speed: 0.07, col: [0.62, 0.82, 1.0], k: 0.70, kind: 0 },
        { n: 90, scale: 27, speed: 0.13, col: [0.62, 0.82, 1.0], k: 0.455, kind: 0 },
        { n: 450, scale: 38, speed: 0.10, col: [1.0, 0.6, 0.15], k: 0.434, kind: 1 },
        { n: 380, scale: 41, speed: 0.12, col: [0.72, 0.5, 1.0], k: 0.248, kind: 1 }
      ];
      var data = [];
      layers.forEach(function (layer) {
        for (var i = 0; i < layer.n; i++) {
          var x = layer.kind ? 0.08 + Math.random() * 0.84 : Math.random();
          data.push(
            x, Math.random(), Math.random(), layer.speed,
            layer.col[0] * layer.k, layer.col[1] * layer.k, layer.col[2] * layer.k, layer.kind,
            (2 * 0.16) / layer.scale
          );
        }
      });
      return { array: new Float32Array(data), count: data.length / 9 };
    }

    var quad = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, quad);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);

    function bindQuad(prog) {
      var loc = gl.getAttribLocation(prog, "a_pos");
      gl.bindBuffer(gl.ARRAY_BUFFER, quad);
      gl.enableVertexAttribArray(loc);
      gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
    }

    var stars = makeStars();
    var starBuf = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, starBuf);
    gl.bufferData(gl.ARRAY_BUFFER, stars.array, gl.STATIC_DRAW);

    createProgramAsync(gl, vertex, nebulaFragment, function (nebProg) {
      if (!nebProg) return;
      createProgramAsync(gl, vertex, blitFragment, function (blitProg) {
        if (!blitProg) return;
        createProgramAsync(gl, starVertex, starFragment, function (starProg) {
          if (!starProg) return;
          createProgramAsync(gl, vertex, grainFragment, function (grainProg) {
            if (!grainProg) return;
            run(nebProg, blitProg, starProg, grainProg);
          });
        });
      });
    });

    function run(nebProg, blitProg, starProg, grainProg) {
      // Two strips: the whole cloud is refreshed every second frame, which on a
      // 165 Hz screen is ~80 updates per second, while each frame only pays for
      // half of the cloud. ?strips=N overrides it for a quick A/B.
      var stripParam = parseInt((/[?&]strips=(\d+)/.exec(location.search) || [])[1], 10);
      var STRIPS = stripParam >= 1 && stripParam <= 16 ? stripParam : 2;
      var blitU = gl.getUniformLocation(blitProg, "u_neb");
      var nebU = {
        res: gl.getUniformLocation(nebProg, "u_res"),
        time: gl.getUniformLocation(nebProg, "u_time"),
        mix: gl.getUniformLocation(nebProg, "u_mix"),
        scroll: gl.getUniformLocation(nebProg, "u_scroll"),
      };
      var starU = {
        res: gl.getUniformLocation(starProg, "u_res"),
        time: gl.getUniformLocation(starProg, "u_time"),
        scroll: gl.getUniformLocation(starProg, "u_scroll"),
        drift: gl.getUniformLocation(starProg, "u_drift")
      };
      var grainU = {
        res: gl.getUniformLocation(grainProg, "u_res"),
        time: gl.getUniformLocation(grainProg, "u_time")
      };
      var starA = {
        a: gl.getAttribLocation(starProg, "a_a"),
        c: gl.getAttribLocation(starProg, "a_c"),
        s: gl.getAttribLocation(starProg, "a_s")
      };

      var target = { texture: null, framebuffer: null, width: 0, height: 0, ready: false };


      // The nebula keeps its own full-resolution buffer and is refreshed one
      // strip per frame, so every frame stays short while the cloud is complete.
      function createTarget() {
        var w = canvas.width;
        var h = canvas.height;
        if (target.texture) gl.deleteTexture(target.texture);
        if (target.framebuffer) gl.deleteFramebuffer(target.framebuffer);
        var texture = gl.createTexture();
        gl.bindTexture(gl.TEXTURE_2D, texture);
        gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, w, h, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
        var framebuffer = gl.createFramebuffer();
        gl.bindFramebuffer(gl.FRAMEBUFFER, framebuffer);
        gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, texture, 0);
        target.ready = gl.checkFramebufferStatus(gl.FRAMEBUFFER) === gl.FRAMEBUFFER_COMPLETE;
        gl.bindFramebuffer(gl.FRAMEBUFFER, null);
        target.texture = texture;
        target.framebuffer = framebuffer;
        target.width = w;
        target.height = h;
      }

      function drawNebulaStrip(now, band, scroll) {
        if (!target.ready) return;
        var strip = Math.ceil(target.height / STRIPS);
        var y = band * strip;
        var h = Math.min(strip, target.height - y);
        if (h <= 0) return;
        gl.bindFramebuffer(gl.FRAMEBUFFER, target.framebuffer);
        gl.viewport(0, y, target.width, h);
        gl.useProgram(nebProg);
        bindQuad(nebProg);
        gl.uniform2f(nebU.res, target.width, target.height);
        gl.uniform1f(nebU.time, now);
        gl.uniform1f(nebU.mix, shaderMix);
        gl.uniform1f(nebU.scroll, scroll || 0);
        gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
        gl.bindFramebuffer(gl.FRAMEBUFFER, null);
      }

      function drawNebulaFull(now) {
        if (!target.ready) return;
        gl.bindFramebuffer(gl.FRAMEBUFFER, target.framebuffer);
        gl.viewport(0, 0, target.width, target.height);
        gl.useProgram(nebProg);
        bindQuad(nebProg);
        gl.uniform2f(nebU.res, target.width, target.height);
        gl.uniform1f(nebU.time, now);
        gl.uniform1f(nebU.mix, shaderMix);
        gl.uniform1f(nebU.scroll, window.scrollY / Math.max(1, window.innerHeight));
        gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
        gl.bindFramebuffer(gl.FRAMEBUFFER, null);
      }

      function resize() {
        var dpr = preferredDpr(1.35);
        var w = Math.max(2, Math.floor(canvas.clientWidth * dpr));
        var h = Math.max(2, Math.floor(canvas.clientHeight * dpr));
        if (canvas.width !== w || canvas.height !== h) {
          canvas.width = w;
          canvas.height = h;
          gl.viewport(0, 0, w, h);
          createTarget();
          drawNebulaFull(0);
        }
      }
      resize();
      window.addEventListener("resize", resize);

      var contextLost = false;
      canvas.addEventListener("webglcontextlost", function (event) {
        event.preventDefault();
        contextLost = true;
        canvas.classList.remove("on");
      });

      var start = performance.now();
      var lastDraw = -Infinity;
      var lastFrame = 0;
      var band = 0;
      var stats = { draws: 0, frames: 0, gaps: [], since: performance.now(), refresh: [] };

      // ?debug=1 shows the live numbers: frame rate, frame interval, canvas size.
      var debugEl = null;
      if (/[?&]debug=1/.test(location.search)) {
        debugEl = document.createElement("div");
        debugEl.style.cssText = "position:fixed;left:12px;bottom:12px;z-index:99999;font:12px/1.5 ui-monospace,SFMono-Regular,Consolas,monospace;color:#bfe9ff;background:rgba(2,6,20,.88);border:1px solid rgba(125,211,252,.35);padding:8px 10px;border-radius:8px;pointer-events:none;white-space:pre";
        document.body.appendChild(debugEl);
      }

      function draw(now, scroll, band) {
        gl.viewport(0, 0, canvas.width, canvas.height);

        // 1) one strip of the nebula buffer, then the whole buffer as a texture
        drawNebulaStrip(now, band, scroll);
        // the strip left its own viewport behind
        gl.viewport(0, 0, canvas.width, canvas.height);

        gl.disable(gl.BLEND);
        gl.useProgram(blitProg);
        bindQuad(blitProg);
        gl.activeTexture(gl.TEXTURE0);
        gl.bindTexture(gl.TEXTURE_2D, target.texture);
        gl.uniform1i(blitU, 0);
        gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);

        // 2) the stars, as additive points
        gl.enable(gl.BLEND);
        gl.blendFunc(gl.ONE, gl.ONE);
        gl.useProgram(starProg);
        gl.bindBuffer(gl.ARRAY_BUFFER, starBuf);
        gl.enableVertexAttribArray(starA.a);
        gl.vertexAttribPointer(starA.a, 4, gl.FLOAT, false, 36, 0);
        gl.enableVertexAttribArray(starA.c);
        gl.vertexAttribPointer(starA.c, 4, gl.FLOAT, false, 36, 16);
        gl.enableVertexAttribArray(starA.s);
        gl.vertexAttribPointer(starA.s, 1, gl.FLOAT, false, 36, 32);
        gl.uniform2f(starU.res, canvas.width, canvas.height);
        gl.uniform1f(starU.time, now);
        gl.uniform1f(starU.scroll, scroll);
        gl.uniform1f(starU.drift, STAR_DRIFT);
        gl.drawArrays(gl.POINTS, 0, stars.count);
        gl.disableVertexAttribArray(starA.a);
        gl.disableVertexAttribArray(starA.c);
        gl.disableVertexAttribArray(starA.s);

        // 3) the grain, one hash per pixel on top
        gl.useProgram(grainProg);
        bindQuad(grainProg);
        gl.uniform2f(grainU.res, canvas.width, canvas.height);
        gl.uniform1f(grainU.time, now);
        gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
        gl.disable(gl.BLEND);
      }

      if (prefersReduced) {
        draw(0, window.scrollY / Math.max(1, window.innerHeight), 0);
        canvas.classList.add("on");
        return;
      }

      function frame(now) {
        var dt = lastFrame ? Math.min(0.1, (now - lastFrame) / 1000) : 1 / 60;
        lastFrame = now;
        stats.frames++;
        if (dt < 0.1) stats.refresh.push(dt * 1000);

        if (window.__bgPaused) {
          requestAnimationFrame(frame);
          return;
        }

        // Draw on every animation frame: requestAnimationFrame already paces
        // this to the display, and on a 144/165 Hz screen a fixed gate makes the
        // stars advance every other frame while the page moves every frame.
        if (!contextLost && !document.hidden) {
          // Track the scroll position exactly: no easing, because any lag
          // between the page and the star layers reads as the stars dragging.
          var scrollNow = window.scrollY / Math.max(1, window.innerHeight);
          if (lastDraw > 0) stats.gaps.push(now - lastDraw);
          lastDraw = now;
          stats.draws++;
          draw((now - start) / 1000, scrollNow, band);
          band = (band + 1) % STRIPS;
          if (!canvas.classList.contains("on")) canvas.classList.add("on");
        }

        if (now - stats.since >= 1000) {
          var median = function (list) {
            if (!list.length) return 0;
            var sorted = list.slice().sort(function (a, b) { return a - b; });
            return +sorted[sorted.length >> 1].toFixed(1);
          };
          window.__bg = {
            fps: Math.round((stats.draws * 1000) / (now - stats.since)),
            framesPerSecond: Math.round((stats.frames * 1000) / (now - stats.since)),
            refreshMs: median(stats.refresh),
            strips: STRIPS,
            gapMedianMs: median(stats.gaps),
            stars: stars.count,
            drift: STAR_DRIFT,
            canvas: [canvas.width, canvas.height],
            viewport: [canvas.clientWidth, canvas.clientHeight]
          };
          if (debugEl) {
            debugEl.textContent =
              "bg draws/s " + window.__bg.fps +
              "\nframe ms " + window.__bg.gapMedianMs +
              "\nrefresh ms " + window.__bg.refreshMs +
              "\nstars " + window.__bg.stars +
              "\ncanvas " + window.__bg.canvas.join("x") +
              "\nviewport " + window.__bg.viewport.join("x");
          }
          stats = { draws: 0, frames: 0, gaps: [], since: now, refresh: [] };
        }

        requestAnimationFrame(frame);
      }
      requestAnimationFrame(frame);
    }
  }

  /* ---------- CRT shader for the hero screenshot ---------- */
  function initCrt() {
    var img = document.querySelector(".crt-fallback");
    var canvas = document.querySelector(".crt-canvas");
    if (!img || !canvas) return;

    var gl = getWebGLContext(canvas, { antialias: false, alpha: false });
    if (!gl) return;

    var vertex = [
      "attribute vec2 a_pos;",
      "varying vec2 v_uv;",
      "void main(){ v_uv = a_pos * 0.5 + 0.5; gl_Position = vec4(a_pos, 0.0, 1.0); }"
    ].join("\n");

    var fragment = [
      "precision highp float;",
      "uniform sampler2D u_tex;",
      "uniform float u_time;",
      "varying vec2 v_uv;",
      "float hash(vec2 p){ return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453123); }",
      "vec2 curve(vec2 uv){",
      "  uv = uv * 2.0 - 1.0;",
      "  vec2 off = abs(uv.yx) / vec2(5.5, 4.5);",
      "  uv += uv * off * off;",
      "  return uv * 0.5 + 0.5;",
      "}",
      "float burstAt(float t, float cycle, float salt){",
      "  float idx = floor(t / cycle);",
      "  float local = mod(t, cycle);",
      "  float start = 0.4 + hash(vec2(idx, salt)) * (cycle - 1.6);",
      "  float dur = 0.07 + hash(vec2(idx, salt + 2.7)) * 0.23;",
      "  return step(start, local) * step(local, start + dur);",
      "}",
      "void main(){",
      "  vec2 uv = curve(v_uv);",
      "  uv = (uv - 0.5) * 0.94 + 0.5;",
      "  if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) { gl_FragColor = vec4(0.0, 0.0, 0.0, 1.0); return; }",
      "  uv.x += sin(uv.y * 38.0 + u_time * 1.7) * 0.0006;",
      "  float tick = floor(u_time * 1.7);",
      "  float tear = smoothstep(0.018, 0.0, abs(uv.y - hash(vec2(tick, 3.0))));",
      "  uv.x += tear * (hash(vec2(tick, 7.0)) - 0.5) * 0.006;",
      "  vec2 d = uv - 0.5;",
      "  float ca = 0.0012 + dot(d, d) * 0.007;",
      "  float r = texture2D(u_tex, uv + vec2(ca, 0.0)).r;",
      "  float g = texture2D(u_tex, uv).g;",
      "  float b = texture2D(u_tex, uv - vec2(ca, 0.0)).b;",
      "  vec3 col = vec3(r, g, b);",
      "  float scan = 0.86 + 0.14 * sin(gl_FragCoord.y * 2.0);",
      "  col *= scan;",
      "  float mx = mod(gl_FragCoord.x, 3.0);",
      "  vec3 mask = vec3(1.0);",
      "  if (mx < 1.0) mask.r = 1.09; else if (mx < 2.0) mask.g = 1.09; else mask.b = 1.09;",
      "  col *= mask;",
      "  float bandPos = fract(u_time * 0.07);",
      "  float band = smoothstep(0.055, 0.0, abs(uv.y - bandPos));",
      "  col += band * vec3(0.55, 0.7, 1.0) * 0.10;",
      "  float seed = floor(u_time * 18.0);",
      "  float n = hash(gl_FragCoord.xy * vec2(0.9, 1.35) + vec2(seed * 37.0, seed * 17.0));",
      "  float n2 = hash(gl_FragCoord.xy * 0.43 + seed * 91.0);",
      "  float streak = step(0.985, hash(vec2(floor(uv.y * 140.0), seed * 1.7))) * 0.35;",
      "  float warm = smoothstep(0.5, 0.78, u_time);",
      "  vec3 staticCol = vec3(n * (0.62 + 0.38 * n2) + streak) * scan * vec3(0.9, 0.96, 1.0);",
      "  float burst = max(burstAt(u_time, 7.3, 1.0), burstAt(u_time, 11.1, 5.0));",
      "  col = mix(col, staticCol, max(1.0 - warm, burst));",
      "  float lockFlash = exp(-pow((u_time - 0.52) * 28.0, 2.0));",
      "  col += lockFlash * 0.12;",
      "  col += (n - 0.5) * 0.05;",
      "  col *= 0.985 + 0.015 * sin(u_time * 41.0);",
      "  vec2 vv = uv * (1.0 - uv);",
      "  float vig = pow(clamp(vv.x * vv.y * 16.0, 0.0, 1.0), 0.2);",
      "  col *= mix(0.72, 1.0, vig);",
      "  float lum = max(col.r, max(col.g, col.b));",
      "  col += smoothstep(0.62, 1.0, lum) * 0.05;",
      "  gl_FragColor = vec4(col, 1.0);",
      "}"
    ].join("\n");

    createProgramAsync(gl, vertex, fragment, function (prog) {
      if (!prog) return;
      gl.useProgram(prog);

    var buf = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, buf);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
    var loc = gl.getAttribLocation(prog, "a_pos");
    gl.enableVertexAttribArray(loc);
    gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);

    var uTex = gl.getUniformLocation(prog, "u_tex");
    var uTime = gl.getUniformLocation(prog, "u_time");
    var ready = false;
    var started = 0;
    var visible = !("IntersectionObserver" in window);
    var rafId = 0;

    function resize() {
      var dpr = preferredDpr(1.35);
      var w = Math.max(2, Math.round(img.clientWidth * dpr));
      var h = Math.max(2, Math.round(w * (img.naturalHeight || 900) / (img.naturalWidth || 1180)));
      if (canvas.width !== w || canvas.height !== h) {
        canvas.width = w;
        canvas.height = h;
        gl.viewport(0, 0, w, h);
      }
    }

    function draw(now) {
      gl.uniform1f(uTime, now);
      gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    }

    function stop() {
      if (rafId) cancelAnimationFrame(rafId);
      rafId = 0;
    }

    var lastCrtDraw = -Infinity;
    function frame(now) {
      rafId = 0;
      if (!ready || !visible || document.hidden) return;
      // The CRT is scanlines and static: 30 fps looks identical and costs half.
      if (now - lastCrtDraw >= 33) {
        lastCrtDraw = now;
        draw((now - started) / 1000);
      }
      rafId = requestAnimationFrame(frame);
    }

    function kick() {
      if (!ready || prefersReduced || !visible || document.hidden || rafId) return;
      rafId = requestAnimationFrame(frame);
    }

    if ("IntersectionObserver" in window) {
      new IntersectionObserver(function (entries) {
        visible = entries[0].isIntersecting;
        if (visible) kick();
        else stop();
      }, { threshold: 0.01 }).observe(canvas);
    }
    document.addEventListener("visibilitychange", function () {
      if (document.hidden) stop();
      else kick();
    });
    canvas.addEventListener("webglcontextlost", function (event) {
      event.preventDefault();
      ready = false;
      stop();
      canvas.classList.remove("on");
      img.classList.remove("crt-off");
    });

    function start() {
      try {
        resize();
        var tex = gl.createTexture();
        gl.bindTexture(gl.TEXTURE_2D, tex);
        gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, true);
        gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, img);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
        gl.uniform1i(uTex, 0);
        canvas.classList.add("on");
        img.classList.add("crt-off");
        ready = true;
        started = performance.now();
        window.addEventListener("resize", resize);
        if (prefersReduced) {
          draw(1.0);
        } else {
          kick();
        }
      } catch (e) {
        canvas.classList.remove("on");
        img.classList.remove("crt-off");
      }
    }

    if (img.complete && img.naturalWidth) {
      start();
    } else {
      img.addEventListener("load", start, { once: true });
    }
    });
  }

  /* ---------- shader fog for final CTA ---------- */
  function initCtaFog() {
    var canvas = document.querySelector(".cta-fog");
    if (!canvas) return;

    var started = false;
    function startFog() {
      if (started) return;
      started = true;

    var gl = getWebGLContext(canvas, { alpha: true, antialias: false, premultipliedAlpha: false });
    if (!gl) return;

    var vertex = [
      "attribute vec2 a_pos;",
      "void main(){ gl_Position = vec4(a_pos, 0.0, 1.0); }"
    ].join("\n");

    var fragment = [
      "precision highp float;",
      "uniform vec2 u_res;",
      "uniform float u_time;",
      "float hash(vec2 p){ return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453123); }",
      "float noise(vec2 p){",
      "  vec2 i = floor(p); vec2 f = fract(p);",
      "  f = f * f * (3.0 - 2.0 * f);",
      "  return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), f.x),",
      "             mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), f.x), f.y);",
      "}",
      "float fbm(vec2 p){",
      "  float v = 0.0; float a = 0.5;",
      "  for (int i = 0; i < 4; i++){ v += a * noise(p); p *= 2.02; a *= 0.5; }",
      "  return v;",
      "}",
      "void main(){",
      "  vec2 uv = gl_FragCoord.xy / u_res.xy;",
      "  vec2 p = (gl_FragCoord.xy - 0.5 * u_res.xy) / u_res.y;",
      "  float t = u_time * 0.035;",
      "  vec2 q = vec2(fbm(p * 1.15 + t), fbm(p * 1.15 + vec2(5.2, 1.3) - t));",
      "  vec2 r = vec2(fbm(p * 1.15 + 2.0 * q + vec2(1.7, 9.2) + 0.12 * t), fbm(p * 1.15 + 2.0 * q + vec2(8.3, 2.8) - 0.1 * t));",
      "  float f = fbm(p * 1.15 + 1.7 * r);",
      "  float fog = smoothstep(0.28, 0.68, f);",
      "  vec2 e = uv * (1.0 - uv);",
      "  float edgeFade = smoothstep(0.0, 0.16, e.x) * smoothstep(0.0, 0.16, e.y);",
      "  float radial = smoothstep(0.9, 0.3, length((uv - 0.5) * vec2(1.2, 1.0)));",
      "  float fade = edgeFade * radial;",
      "  vec3 col = mix(vec3(0.05, 0.13, 0.24), vec3(0.12, 0.30, 0.36), f * 0.55);",
      "  col += (hash(gl_FragCoord.xy + u_time) - 0.5) * 0.015;",
      "  gl_FragColor = vec4(col, fog * fade * 0.4);",
      "}"
    ].join("\n");

    createProgramAsync(gl, vertex, fragment, function (prog) {
      if (!prog) return;
      gl.useProgram(prog);

    var buf = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, buf);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
    var loc = gl.getAttribLocation(prog, "a_pos");
    gl.enableVertexAttribArray(loc);
    gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
    var uRes = gl.getUniformLocation(prog, "u_res");
    var uTime = gl.getUniformLocation(prog, "u_time");

    function resize() {
      var dpr = preferredDpr(1.25);
      var w = Math.max(2, Math.round(canvas.clientWidth * dpr));
      var h = Math.max(2, Math.round(canvas.clientHeight * dpr));
      if (canvas.width !== w || canvas.height !== h) {
        canvas.width = w;
        canvas.height = h;
        gl.viewport(0, 0, w, h);
      }
    }
    resize();
    window.addEventListener("resize", resize);
    gl.clearColor(0, 0, 0, 0);
    var visible = false;
    var rafId = 0;

    function stop() {
      if (rafId) cancelAnimationFrame(rafId);
      rafId = 0;
    }

    function kick() {
      if (prefersReduced || !visible || document.hidden || rafId) return;
      rafId = requestAnimationFrame(frame);
    }

    canvas.addEventListener("webglcontextlost", function (event) {
      event.preventDefault();
      visible = false;
      stop();
    });

    function draw(t) {
      gl.clear(gl.COLOR_BUFFER_BIT);
      gl.uniform2f(uRes, canvas.width, canvas.height);
      gl.uniform1f(uTime, t);
      gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    }

    if ("IntersectionObserver" in window) {
      new IntersectionObserver(function (entries) {
        visible = entries[0].isIntersecting;
        if (visible) kick();
        else stop();
      }, { threshold: 0 }).observe(canvas);
    } else {
      visible = true;
    }

    if (prefersReduced) {
      draw(0);
      return;
    }
    var start = performance.now();
    var lastDraw = -Infinity;
    function frame(now) {
      rafId = 0;
      if (visible && !document.hidden && now - lastDraw >= 32) {
        draw((now - start) / 1000);
        lastDraw = now;
      }
      if (visible && !document.hidden) rafId = requestAnimationFrame(frame);
    }
    document.addEventListener("visibilitychange", function () {
      if (document.hidden) stop();
      else kick();
    });
    kick();
    });
    }

    if ("IntersectionObserver" in window) {
      var lazyObserver = new IntersectionObserver(function (entries) {
        if (!entries[0].isIntersecting) return;
        lazyObserver.disconnect();
        startFog();
      }, { rootMargin: "300px 0px" });
      lazyObserver.observe(canvas);
    } else {
      startFog();
    }
  }

  /* ---------- interlude camera ---------- */
  function initInterludeCamera() {
    var heroCam = document.querySelector(".shape-cam .cam3d");
    var mount = document.querySelector(".interlude-cam");
    if (!heroCam || !mount) return;

    var cam = heroCam.cloneNode(true);
    mount.appendChild(cam);

    var pointer = { x: window.innerWidth / 2, y: window.innerHeight / 2, active: false };
    window.addEventListener("pointermove", function (e) {
      pointer.x = e.clientX;
      pointer.y = e.clientY;
      pointer.active = true;
    }, { passive: true });
    document.documentElement.addEventListener("pointerleave", function () {
      pointer.active = false;
    });
    window.addEventListener("blur", function () {
      pointer.active = false;
    });

    var visible = !("IntersectionObserver" in window);
    var rafId = 0;

    function stop() {
      if (rafId) cancelAnimationFrame(rafId);
      rafId = 0;
    }

    function kick() {
      if (prefersReduced || !visible || document.hidden || rafId) return;
      measureMount();
      rafId = requestAnimationFrame(frame);
    }

    if ("IntersectionObserver" in window) {
      new IntersectionObserver(function (entries) {
        visible = entries[0].isIntersecting;
        if (visible) kick();
        else stop();
      }, { threshold: 0 }).observe(mount);
    }
    document.addEventListener("visibilitychange", function () {
      if (document.hidden) {
        pointer.active = false;
        stop();
      } else {
        kick();
      }
    });

    window.addEventListener("resize", function () {
      if (visible) measureMount();
    }, { passive: true });

    if (prefersReduced) {
      cam.style.transform = "rotateY(-8deg) rotateX(4deg)";
      return;
    }

    var yaw = 0;
    var pitch = 0;
    var start = performance.now();
    var flourish = {
      start: -Infinity,
      duration: 1080,
      direction: 1,
      radiusX: 26,
      radiusY: 11,
      tilt: 5,
      pitch: 6,
      lift: 0.08
    };
    mount.addEventListener("camera-flourish", function () {
      var direction = Math.random() < 0.5 ? -1 : 1;
      flourish = {
        start: performance.now(),
        duration: 920 + Math.random() * 300,
        direction: direction,
        radiusX: 19 + Math.random() * 16,
        radiusY: 8 + Math.random() * 11,
        tilt: 3.5 + Math.random() * 5.5,
        pitch: 3 + Math.random() * 7,
        lift: 0.055 + Math.random() * 0.07
      };
    });

    var mountBox = { left: 0, top: 0, width: 0, height: 0 };
    function measureMount() {
      var r = mount.getBoundingClientRect();
      mountBox = { left: r.left + window.scrollX, top: r.top + window.scrollY, width: r.width, height: r.height };
    }

    function frame(now) {
      rafId = 0;
      if (visible && !document.hidden) {
        var cx = mountBox.left + mountBox.width / 2 - window.scrollX;
        var cy = mountBox.top + mountBox.height / 2 - window.scrollY;
        var dx = pointer.x - cx;
        var dy = pointer.y - cy;
        if (!pointer.active || Math.hypot(dx, dy) > 650) {
          dx = window.innerWidth / 2 - cx;
          dy = window.innerHeight / 2 - cy;
        }
        var rangeX = Math.max(360, window.innerWidth * 0.45);
        var rangeY = Math.max(260, window.innerHeight * 0.45);
        var targetYaw = Math.max(-29, Math.min(29, dx / rangeX * 29));
        var targetPitch = Math.max(-16, Math.min(16, -dy / rangeY * 16));
        var t = (now - start) / 1000;
        var ease = 0.045 + 0.025 * (0.5 + 0.5 * Math.sin(t * 1.4));
        yaw += (targetYaw - yaw) * ease;
        pitch += (targetPitch - pitch) * ease;
        var swayYaw = Math.sin(t * 0.7) * 1.8;
        var swayPitch = Math.sin(t * 0.9 + 1.1) * 1.1;
        var bob = Math.sin(t * 0.8) * 4;
        var elapsed = now - flourish.start;
        var celebrating = elapsed >= 0 && elapsed < flourish.duration;
        var flourishX = 0;
        var flourishY = 0;
        var flourishYaw = 0;
        var flourishTilt = 0;
        var flourishPitch = 0;
        var flourishScale = 1;
        if (celebrating) {
          var progress = elapsed / flourish.duration;
          var orbit = progress * Math.PI * 2 * flourish.direction;
          var spin = progress < 0.5
            ? 4 * Math.pow(progress, 3)
            : 1 - Math.pow(-2 * progress + 2, 3) / 2;
          flourishX = Math.sin(orbit) * flourish.radiusX;
          flourishY = (Math.cos(orbit) - 1) * flourish.radiusY;
          flourishYaw = spin * 360 * flourish.direction;
          flourishTilt = Math.sin(orbit) * flourish.tilt;
          flourishPitch = Math.sin(orbit * 2) * flourish.pitch;
          flourishScale = 1 + Math.sin(progress * Math.PI) * flourish.lift;
          mount.classList.add("is-celebrating");
        } else {
          mount.classList.remove("is-celebrating");
        }
        cam.style.transform = "translate3d(" + flourishX.toFixed(2) + "px," + (bob + flourishY).toFixed(2) + "px,0) rotateY(" + (yaw + swayYaw + flourishYaw).toFixed(2) + "deg) rotateX(" + (pitch + swayPitch + flourishPitch).toFixed(2) + "deg) rotateZ(" + flourishTilt.toFixed(2) + "deg) scale(" + flourishScale.toFixed(3) + ")";
      }
      if (visible && !document.hidden) rafId = requestAnimationFrame(frame);
    }
    kick();
  }

  function initCameraShutters() {
    var flash = document.querySelector(".shutter-flash");
    var audioContext = null;
    var readyAt = -Infinity;
    var cooldownMs = 420;
    var celebrationLockMs = 1500;
    var comboWindowMs = 1400;
    var comboCount = 3;
    var shotTimes = [];

    function runAudio(start) {
      var AudioCtor = window.AudioContext || window.webkitAudioContext;
      if (!AudioCtor) return;
      if (!audioContext) audioContext = new AudioCtor();
      if (audioContext.state === "suspended") {
        audioContext.resume().then(start).catch(function () {});
      } else {
        start();
      }
    }

    function playShutter() {
      function playBursts() {
        var now = audioContext.currentTime + 0.006;

        function burst(start, duration, level, cutoff) {
          var length = Math.max(1, Math.ceil(audioContext.sampleRate * duration));
          var buffer = audioContext.createBuffer(1, length, audioContext.sampleRate);
          var samples = buffer.getChannelData(0);
          for (var i = 0; i < length; i += 1) {
            var envelope = Math.pow(1 - i / length, 3);
            samples[i] = (Math.random() * 2 - 1) * envelope;
          }
          var source = audioContext.createBufferSource();
          var filter = audioContext.createBiquadFilter();
          var gain = audioContext.createGain();
          source.buffer = buffer;
          filter.type = "highpass";
          filter.frequency.setValueAtTime(cutoff, start);
          gain.gain.setValueAtTime(level, start);
          gain.gain.exponentialRampToValueAtTime(0.0001, start + duration);
          source.connect(filter);
          filter.connect(gain);
          gain.connect(audioContext.destination);
          source.start(start);
          source.stop(start + duration);
        }

        function bodyClick(start, fromHz, toHz, duration, level) {
          var oscillator = audioContext.createOscillator();
          var gain = audioContext.createGain();
          oscillator.type = "triangle";
          oscillator.frequency.setValueAtTime(fromHz, start);
          oscillator.frequency.exponentialRampToValueAtTime(toHz, start + duration);
          gain.gain.setValueAtTime(level, start);
          gain.gain.exponentialRampToValueAtTime(0.0001, start + duration);
          oscillator.connect(gain);
          gain.connect(audioContext.destination);
          oscillator.start(start);
          oscillator.stop(start + duration);
        }

        burst(now, 0.034, 0.38, 1180);
        bodyClick(now, 190, 72, 0.055, 0.13);
        burst(now + 0.078, 0.026, 0.23, 2050);
        bodyClick(now + 0.078, 150, 88, 0.04, 0.075);
      }

      runAudio(playBursts);
    }

    function playFlourish() {
      function playWhoosh() {
        var start = audioContext.currentTime + 0.22;
        var duration = 0.72;
        var length = Math.ceil(audioContext.sampleRate * duration);
        var buffer = audioContext.createBuffer(1, length, audioContext.sampleRate);
        var samples = buffer.getChannelData(0);
        for (var i = 0; i < length; i += 1) {
          var envelope = Math.sin(Math.PI * i / length);
          samples[i] = (Math.random() * 2 - 1) * envelope * envelope;
        }

        var source = audioContext.createBufferSource();
        var filter = audioContext.createBiquadFilter();
        var gain = audioContext.createGain();
        source.buffer = buffer;
        filter.type = "bandpass";
        filter.Q.setValueAtTime(0.9, start);
        filter.frequency.setValueAtTime(520, start);
        filter.frequency.exponentialRampToValueAtTime(2700, start + 0.3);
        filter.frequency.exponentialRampToValueAtTime(860, start + duration);
        gain.gain.setValueAtTime(0.0001, start);
        gain.gain.linearRampToValueAtTime(0.12, start + 0.22);
        gain.gain.exponentialRampToValueAtTime(0.0001, start + duration);
        source.connect(filter);
        filter.connect(gain);

        var panner = null;
        if (audioContext.createStereoPanner) {
          panner = audioContext.createStereoPanner();
          panner.pan.setValueAtTime(-0.34, start);
          panner.pan.linearRampToValueAtTime(0.38, start + 0.34);
          panner.pan.linearRampToValueAtTime(0, start + duration);
          gain.connect(panner);
          panner.connect(audioContext.destination);
        } else {
          gain.connect(audioContext.destination);
        }
        source.start(start);
        source.stop(start + duration);

        var glide = audioContext.createOscillator();
        var glideGain = audioContext.createGain();
        glide.type = "sine";
        glide.frequency.setValueAtTime(210, start);
        glide.frequency.exponentialRampToValueAtTime(920, start + 0.3);
        glide.frequency.exponentialRampToValueAtTime(410, start + duration);
        glideGain.gain.setValueAtTime(0.0001, start);
        glideGain.gain.linearRampToValueAtTime(0.045, start + 0.24);
        glideGain.gain.exponentialRampToValueAtTime(0.0001, start + duration);
        glide.connect(glideGain);
        glideGain.connect(audioContext.destination);
        glide.start(start);
        glide.stop(start + duration);
      }

      runAudio(playWhoosh);
    }

    function fire(trigger) {
      var now = performance.now();
      if (now < readyAt) return;
      readyAt = now + cooldownMs;
      playShutter();

      trigger.classList.remove("is-firing");
      void trigger.offsetWidth;
      trigger.classList.add("is-firing");
      trigger.setAttribute("aria-disabled", "true");

      if (flash) {
        flash.classList.remove("is-firing");
        void flash.offsetWidth;
        flash.classList.add("is-firing");
      }

      shotTimes = shotTimes.filter(function (time) {
        return now - time <= comboWindowMs;
      });
      shotTimes.push(now);
      if (shotTimes.length >= comboCount) {
        shotTimes.length = 0;
        readyAt = now + celebrationLockMs;
        playFlourish();
        window.setTimeout(function () {
          trigger.dispatchEvent(new Event("camera-flourish"));
        }, 220);
      }

      window.setTimeout(function () {
        trigger.classList.remove("is-firing");
      }, cooldownMs);

      window.setTimeout(function () {
        trigger.removeAttribute("aria-disabled");
      }, Math.max(cooldownMs, readyAt - now));
    }

    document.querySelectorAll("[data-camera-shutter]").forEach(function (trigger) {
      trigger.addEventListener("click", function () {
        fire(trigger);
      });
      trigger.addEventListener("keydown", function (event) {
        if (event.key !== "Enter" && event.key !== " ") return;
        event.preventDefault();
        fire(trigger);
      });
    });
  }

  function initFooterPeekCamera() {
    var heroCam = document.querySelector(".shape-cam .cam3d");
    var downloadButton = document.querySelector(".final-cta .btn-primary");
    var mount = document.querySelector("[data-footer-peek]");
    var trigger = mount && mount.querySelector(".footer-peek-trigger");
    if (!heroCam || !downloadButton || !mount || !trigger) return;

    var cam = heroCam.cloneNode(true);
    trigger.appendChild(cam);

    var dwellMs = 30000;
    var hideMinMs = 30000;
    var hideMaxMs = 50000;
    var bottomSince = 0;
    var hiddenUntil = 0;
    var visible = false;
    var retreating = false;
    var side = "left";
    var activeStart = 0;
    var yaw = 0;
    var pitch = 0;
    var kickCam = function () {};
    var stopCam = function () {};
    var pointer = { x: window.innerWidth / 2, y: window.innerHeight / 2, active: false };

    function isInPeekZone() {
      var rect = downloadButton.getBoundingClientRect();
      var visibleHeight = Math.min(rect.bottom, window.innerHeight) - Math.max(rect.top, 0);
      return visibleHeight >= rect.height * 0.55 && rect.bottom > 70;
    }

    function show() {
      if (visible || retreating || performance.now() < hiddenUntil) return;
      side = Math.random() < 0.5 ? "left" : "right";
      visible = true;
      activeStart = performance.now();
      yaw = side === "left" ? 20 : -20;
      pitch = 0;
      mount.classList.remove("is-left", "is-right", "is-retreating");
      void mount.offsetWidth;
      mount.classList.add("is-peeking", "is-" + side);
      mount.setAttribute("aria-hidden", "false");
      trigger.tabIndex = 0;
      if (prefersReduced) cam.style.transform = "rotateY(" + yaw + "deg) rotateX(-3deg)";
      else kickCam();
    }

    function hide(cooldown) {
      if (!visible || retreating) return;
      retreating = true;
      if (cooldown > 0) hiddenUntil = performance.now() + cooldown;
      mount.setAttribute("aria-hidden", "true");
      trigger.tabIndex = -1;
      mount.classList.add("is-retreating");
      stopCam();
      window.setTimeout(function () {
        visible = false;
        retreating = false;
        mount.classList.remove("is-peeking", "is-left", "is-right", "is-retreating");
      }, 300);
    }

    function escape() {
      hide(hideMinMs + Math.random() * (hideMaxMs - hideMinMs));
    }

    function updatePresence() {
      if (!isInPeekZone()) {
        bottomSince = 0;
        if (visible) hide(0);
      } else if (!bottomSince) {
        bottomSince = performance.now();
      }
    }

    trigger.addEventListener("click", function () {
      escape();
    });
    trigger.addEventListener("pointerenter", function () {
      escape();
    });

    window.addEventListener("scroll", updatePresence, { passive: true });
    window.addEventListener("resize", updatePresence);
    document.addEventListener("visibilitychange", function () {
      if (document.hidden) {
        if (visible) hide(0);
        stopCam();
      } else {
        updatePresence();
        if (visible) kickCam();
      }
    });

    window.addEventListener("pointermove", function (event) {
      pointer.x = event.clientX;
      pointer.y = event.clientY;
      pointer.active = true;
      if (visible && !retreating) {
        var rect = trigger.getBoundingClientRect();
        var cx = rect.left + rect.width / 2;
        var cy = rect.top + rect.height / 2;
        var distance = Math.hypot(event.clientX - cx, event.clientY - cy);
        if (distance < 170) escape();
      }
    }, { passive: true });

    window.setInterval(function () {
      if (document.hidden) return;
      updatePresence();
      var now = performance.now();
      if (!visible && !retreating && now >= hiddenUntil && bottomSince && now - bottomSince >= dwellMs) show();
    }, 500);

    if (!prefersReduced) {
      var camRaf = 0;
      function frame(now) {
        camRaf = 0;
        if (visible && !document.hidden) {
          var rect = trigger.getBoundingClientRect();
          var cx = rect.left + rect.width / 2;
          var cy = rect.top + rect.height / 2;
          var targetYaw = side === "left" ? 22 : -22;
          var targetPitch = -3;
          if (pointer.active) {
            targetYaw = Math.max(-30, Math.min(30, (pointer.x - cx) / 220 * 28));
            targetPitch = Math.max(-11, Math.min(9, -(pointer.y - cy) / 190 * 10));
          }
          yaw += (targetYaw - yaw) * 0.2;
          pitch += (targetPitch - pitch) * 0.18;
          var t = (now - activeStart) / 1000;
          var scanYaw = Math.sin(t * 0.72) * 3.6 + Math.sin(t * 1.37) * 1.4;
          var scanPitch = Math.sin(t * 0.93 + 0.7) * 1.5;
          var bob = Math.sin(t * 0.88) * 3 + Math.sin(t * 1.71) * 1.2;
          cam.style.transform = "rotateY(" + (yaw + scanYaw).toFixed(2) + "deg) rotateX(" + (pitch + scanPitch).toFixed(2) + "deg) translateY(" + bob.toFixed(2) + "px)";
        }
        if (visible && !document.hidden) camRaf = requestAnimationFrame(frame);
      }
      kickCam = function () {
        if (!visible || document.hidden || camRaf) return;
        camRaf = requestAnimationFrame(frame);
      };
      stopCam = function () {
        if (camRaf) cancelAnimationFrame(camRaf);
        camRaf = 0;
      };
    }

    updatePresence();
  }

  /* ---------- nav ---------- */
  function initNav() {
    var nav = document.getElementById("nav");
    if (!nav) return;
    var onScroll = function () {
      nav.classList.toggle("scrolled", window.scrollY > 24);
    };
    window.addEventListener("scroll", onScroll, { passive: true });
    onScroll();
  }

  /* ---------- reveal ---------- */
  function initReveal() {
    var items = document.querySelectorAll(".reveal");
    if (!("IntersectionObserver" in window)) {
      items.forEach(function (el) { el.classList.add("in"); });
      return;
    }
    var io = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (entry.isIntersecting) {
          var el = entry.target;
          var delay = parseInt(el.getAttribute("data-delay") || "0", 10);
          el.style.setProperty("--delay", delay + "ms");
          el.classList.add("in");
          if (el.hasAttribute("data-scramble")) {
            scrambleTo(el, el.textContent);
          }
          io.unobserve(el);
        }
      });
    }, { threshold: 0.12, rootMargin: "0px 0px -40px 0px" });
    items.forEach(function (el) { io.observe(el); });
  }

  /* ---------- counters ---------- */
  function initCounters() {
    var counters = document.querySelectorAll("[data-count]");
    if (!counters.length) return;
    if (!("IntersectionObserver" in window)) {
      counters.forEach(function (el) {
        el.textContent = el.getAttribute("data-count");
      });
      return;
    }
    var io = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (!entry.isIntersecting) return;
        var el = entry.target;
        var target = parseInt(el.getAttribute("data-count"), 10);
        io.unobserve(el);
        if (prefersReduced) {
          el.textContent = target;
          return;
        }
        var start = performance.now();
        var dur = 1400;
        var step = function (now) {
          var tt = Math.min((now - start) / dur, 1);
          var eased = 1 - Math.pow(1 - tt, 3);
          el.textContent = Math.round(target * eased);
          if (tt < 1) requestAnimationFrame(step);
        };
        requestAnimationFrame(step);
      });
    }, { threshold: 0.6 });
    counters.forEach(function (el) { io.observe(el); });
  }

  /* ---------- REC timer ---------- */
  function initRecTimer() {
    var el = document.getElementById("recTime");
    if (!el || prefersReduced) return;
    var seconds = 0;
    setInterval(function () {
      if (document.hidden) return;
      seconds++;
      var m = Math.floor(seconds / 60);
      var s = seconds % 60;
      el.textContent = (m < 10 ? "0" + m : m) + ":" + (s < 10 ? "0" + s : s);
    }, 1000);
  }

  /* ---------- conversion progress ---------- */
  function initConversionProgress() {
    var card = document.querySelector(".convert-card");
    var fill = document.querySelector(".convert-progress i");
    var percent = document.getElementById("convertPercent");
    if (!card || !fill || !percent) return;

    var target = 85;
    var duration = 5200;
    var started = false;
    var lastPct = -1;
    var percentDigits = percent.querySelectorAll(".wobble-letter");

    function setValue(value) {
      fill.style.setProperty("--p", value.toFixed(2) + "%");
      var pct = Math.round(value);
      if (pct !== lastPct) {
        var text = String(pct);
        if (percentDigits[0]) percentDigits[0].textContent = text.length > 1 ? text[0] : "";
        if (percentDigits[1]) percentDigits[1].textContent = text.length > 1 ? text[1] : text[0];
        lastPct = pct;
      }
    }

    function run() {
      if (started) return;
      started = true;
      if (prefersReduced) {
        setValue(target);
        return;
      }
      var start = performance.now();
      function frame(now) {
        var t = Math.min((now - start) / duration, 1);
        var eased = 1 - Math.pow(1 - t, 3);
        setValue(target * eased);
        if (t < 1) {
          requestAnimationFrame(frame);
        } else {
          setValue(target);
        }
      }
      requestAnimationFrame(frame);
    }

    if ("IntersectionObserver" in window) {
      var io = new IntersectionObserver(function (entries) {
        if (entries[0].isIntersecting) {
          run();
          io.disconnect();
        }
      }, { threshold: 0.35 });
      io.observe(card);
    } else {
      run();
    }
  }

  /* ---------- 3D tilt ---------- */
  function initTilt() {
    if (prefersReduced || !window.matchMedia("(hover: hover)").matches) return;
    var items = document.querySelectorAll("[data-tilt]");
    items.forEach(function (el) {
      var media = el.querySelector(".pillar-shot, .bento-media");
      var frameId = 0;
      var pending = null;

      function render() {
        frameId = 0;
        if (!pending) return;
        var px = pending.x;
        var py = pending.y;
        pending = null;
        var rx = -py * 2.4;
        var ry = px * 2.4;
        el.style.transform = "perspective(900px) rotateX(" + rx.toFixed(2) + "deg) rotateY(" + ry.toFixed(2) + "deg) translateY(-2px)";
        if (media) {
          media.style.setProperty("--tilt-angle", (rx * 3).toFixed(2) + "deg");
          media.style.setProperty("--mask-shift", (ry * 4).toFixed(2) + "%");
          media.style.setProperty("--light-o", "1");
        }
      }

      el.addEventListener("pointermove", function (e) {
        var r = el.getBoundingClientRect();
        pending = {
          x: (e.clientX - r.left) / r.width - 0.5,
          y: (e.clientY - r.top) / r.height - 0.5
        };
        if (!frameId) frameId = requestAnimationFrame(render);
      });
      el.addEventListener("pointerleave", function () {
        pending = null;
        if (frameId) cancelAnimationFrame(frameId);
        frameId = 0;
        el.style.transform = "";
        if (media) media.style.setProperty("--light-o", "0");
      });
    });
  }

  /* ---------- magnetic buttons ---------- */
  function initMagnetic() {
    if (prefersReduced || !window.matchMedia("(hover: hover)").matches) return;
    document.querySelectorAll(".magnetic").forEach(function (btn) {
      var frameId = 0;
      var pending = null;

      function render() {
        frameId = 0;
        if (!pending) return;
        btn.style.transform = "translate(" + pending.x * 0.18 + "px," + pending.y * 0.24 + "px)";
        pending = null;
      }

      btn.addEventListener("pointermove", function (e) {
        var r = btn.getBoundingClientRect();
        pending = {
          x: e.clientX - r.left - r.width / 2,
          y: e.clientY - r.top - r.height / 2
        };
        if (!frameId) frameId = requestAnimationFrame(render);
      });
      btn.addEventListener("pointerleave", function () {
        pending = null;
        if (frameId) cancelAnimationFrame(frameId);
        frameId = 0;
        btn.style.transform = "";
      });
    });
  }

  /* ---------- palette shift on scroll ---------- */
  function hexToRgb(hex) {
    hex = hex.replace("#", "");
    if (hex.length === 3) hex = hex.split("").map(function (c) { return c + c; }).join("");
    var n = parseInt(hex, 16);
    return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
  }

  function lerpColor(a, b, t) {
    var A = hexToRgb(a);
    var B = hexToRgb(b);
    return "rgb(" + Math.round(A[0] + (B[0] - A[0]) * t) + "," + Math.round(A[1] + (B[1] - A[1]) * t) + "," + Math.round(A[2] + (B[2] - A[2]) * t) + ")";
  }

  function initPaletteShift() {
    var root = document.documentElement;
    var pairs = {
      "--g1": ["#1d4ed8", "#4c1d95"],
      "--g2": ["#3b82f6", "#7c3aed"],
      "--g3": ["#22d3ee", "#a855f7"],
      "--g4": ["#7dd3fc", "#c4b5fd"],
      "--cyan": ["#22d3ee", "#a855f7"]
    };
    var frameId = 0;
    // Writing a custom property on <html> invalidates the style of the whole
    // document, so the CSS palette only moves in small steps: a few dozen style
    // recalcs over the whole page instead of one per scroll frame.
    var STEPS = 24;
    var lastStep = -1;
    var max = 1;

    function measure() {
      max = Math.max(1, document.documentElement.scrollHeight - window.innerHeight);
    }

    function render() {
      frameId = 0;
      var raw = Math.min(Math.max(window.scrollY / max, 0), 1);
      // The shader colour can follow continuously, it is just a uniform.
      shaderMix = raw * 0.3;
      var step = Math.round(shaderMix * STEPS);
      if (step === lastStep) return;
      lastStep = step;
      var p = step / STEPS;
      Object.keys(pairs).forEach(function (key) {
        root.style.setProperty(key, lerpColor(pairs[key][0], pairs[key][1], p));
      });
    }

    function update() {
      if (!frameId) frameId = requestAnimationFrame(render);
    }
    window.addEventListener("scroll", update, { passive: true });
    window.addEventListener("resize", function () {
      measure();
      update();
    });
    measure();
    update();
  }

  /* ---------- scroll benchmark (?bench=1) ---------- */
  function runBenchmark() {
    var box = document.createElement("div");
    box.style.cssText = "position:fixed;left:12px;top:12px;z-index:99999;font:12px/1.6 ui-monospace,Consolas,monospace;color:#bfe9ff;background:rgba(2,6,20,.92);border:1px solid rgba(125,211,252,.35);padding:10px 12px;border-radius:8px;white-space:pre";
    box.textContent = "scroll benchmark running...";
    document.body.appendChild(box);

    function measure(duration) {
      return new Promise(function (resolve) {
        var gaps = [];
        var t0 = performance.now();
        var last = t0;
        var max = Math.max(1, document.documentElement.scrollHeight - window.innerHeight);
        function step(now) {
          var t = (now - t0) / duration;
          window.scrollTo(0, Math.min(max, t * max));
          gaps.push(now - last);
          last = now;
          if (t < 1) {
            requestAnimationFrame(step);
          } else {
            var s = gaps.slice(2).sort(function (a, b) { return a - b; });
            resolve({
              fps: Math.round(gaps.length / (duration / 1000)),
              p50: +s[Math.floor(s.length * 0.5)].toFixed(1),
              p90: +s[Math.floor(s.length * 0.9)].toFixed(1)
            });
          }
        }
        requestAnimationFrame(step);
      });
    }

    function wait(ms) {
      return new Promise(function (r) { setTimeout(r, ms); });
    }

    function line(name, r) {
      return name + ": " + r.fps + " fps, p50 " + r.p50 + " ms, p90 " + r.p90 + " ms";
    }

    return (async function () {
      var canvas = document.getElementById("shader");
      window.scrollTo(0, 0);
      await wait(400);
      var normal = await measure(1600);
      await wait(400);
      window.__bgPaused = true;
      var paused = await measure(1600);
      window.scrollTo(0, 0);
      await wait(400);
      window.__bgPaused = false;
      canvas.style.display = "none";
      var hidden = await measure(1600);
      canvas.style.display = "";
      window.scrollTo(0, 0);
      box.textContent = [
        "scroll, background drawing:  " + line("", normal).replace(/^: /, ""),
        "scroll, background paused:   " + line("", paused).replace(/^: /, ""),
        "scroll, background removed:  " + line("", hidden).replace(/^: /, ""),
        "",
        "if 1 and 3 are equal -> the lag is not the background"
      ].join("\n");
      window.__benchResult = { normal: normal, paused: paused, hidden: hidden };
      return window.__benchResult;
    })();
  }

  if (/[?&]bench=1/.test(location.search)) {
    window.addEventListener("load", function () {
      setTimeout(runBenchmark, 1200);
    });
  }

  /* ---------- run ---------- */
  document.querySelectorAll("[data-wobble]").forEach(applyWobble);
  initHeroIntro();
  initNav();
  initReveal();
  initCounters();
  initRecTimer();
  initConversionProgress();
  initTilt();
  initMagnetic();
  initPaletteShift();
  initCameraShutters();
  initCtaFog();

  requestAnimationFrame(function () {
    requestAnimationFrame(function () {
      scheduleIdle(initShader, 900);
      window.setTimeout(function () {
        scheduleIdle(initCrt, 1400);
      }, 120);
      window.setTimeout(function () {
        initInterludeCamera();
        window.setTimeout(initFooterPeekCamera, 80);
      }, 180);
    });
  });
})();

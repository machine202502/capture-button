/*
 * Capture Button — soft caustic light for the page background.
 * A single full-screen WebGL fragment shader, no libraries and no network.
 * It renders one still frame when prefers-reduced-motion or Save-Data is on.
 */
(function () {
  "use strict";

  var canvas = document.getElementById("ocean");
  if (!canvas) return;

  var reduceQuery = window.matchMedia("(prefers-reduced-motion: reduce)");
  var connection = navigator.connection || navigator.mozConnection || navigator.webkitConnection;
  var saveData = Boolean(connection && connection.saveData);

  var gl = null;
  try {
    gl = canvas.getContext("webgl", {
      alpha: false,
      antialias: false,
      depth: false,
      stencil: false,
      powerPreference: "low-power",
      preserveDrawingBuffer: false,
    });
  } catch (error) {
    gl = null;
  }

  if (!gl) {
    canvas.hidden = true;
    document.documentElement.classList.add("no-webgl");
    return;
  }

  var VERTEX = [
    "attribute vec2 aPos;",
    "void main() {",
    "  gl_Position = vec4(aPos, 0.0, 1.0);",
    "}",
  ].join("\n");

  var FRAGMENT = [
    "precision highp float;",
    "uniform vec2 uRes;",
    "uniform float uTime;",
    "uniform vec2 uMouse;",
    "uniform float uMouseStrength;",
    "",
    "float hash(vec2 p) {",
    "  return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453123);",
    "}",
    "",
    "float noise(vec2 p) {",
    "  vec2 i = floor(p);",
    "  vec2 f = fract(p);",
    "  vec2 u = f * f * (3.0 - 2.0 * f);",
    "  return mix(",
    "    mix(hash(i), hash(i + vec2(1.0, 0.0)), u.x),",
    "    mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), u.x),",
    "    u.y",
    "  );",
    "}",
    "",
    "float fbm(vec2 p) {",
    "  float value = 0.0;",
    "  float amplitude = 0.5;",
    "  for (int i = 0; i < 4; i++) {",
    "    value += amplitude * noise(p);",
    "    p *= 2.03;",
    "    amplitude *= 0.5;",
    "  }",
    "  return value;",
    "}",
    "",
    "void main() {",
    "  vec2 uv = gl_FragCoord.xy / uRes.xy;",
    "  vec2 p = (gl_FragCoord.xy * 2.0 - uRes.xy) / uRes.y;",
    "  float t = uTime * 0.045;",
    "",
    "  // Slow domain warp makes the caustics drift instead of loop visibly.",
    "  vec2 q = vec2(",
    "    fbm(p * 1.2 + vec2(t * 0.30, -t * 0.20)),",
    "    fbm(p * 1.2 + vec2(5.2, 1.3) - vec2(t * 0.20, -t * 0.25))",
    "  );",
    "  vec2 r = p + (q - 0.5) * 0.55;",
    "",
    "  float caustic = 0.0;",
    "  for (int i = 0; i < 3; i++) {",
    "    vec2 s = r * (1.7 + float(i) * 0.65) +",
    "      vec2(t * (0.60 + float(i) * 0.20), -t * (0.40 + float(i) * 0.15));",
    "    float wave = sin(s.x + sin(s.y * 1.7 + t * 0.8));",
    "    caustic += pow(abs(wave), 5.0) / (1.0 + float(i));",
    "  }",
    "  caustic = clamp(caustic * 0.45, 0.0, 1.0);",
    "",
    "  float band = smoothstep(0.15, 0.95, q.y + 0.35 * sin(p.x * 1.4 + t * 1.7));",
    "",
    "  vec3 color = vec3(0.004, 0.014, 0.024);",
    "  color += vec3(0.015, 0.090, 0.120) * q.x;",
    "  color += vec3(0.030, 0.300, 0.400) * caustic * 0.22;",
    "  color += vec3(0.000, 0.130, 0.180) * band * 0.22;",
    "",
    "  color += vec3(0.020, 0.220, 0.300) *",
    "    exp(-length(p - vec2(0.85, 0.55)) * 1.45) * 0.30;",
    "  color += vec3(0.010, 0.140, 0.180) *",
    "    exp(-length(p - vec2(-0.95, -0.35)) * 1.20) * 0.35;",
    "",
    "  float mouseGlow = exp(-length(p - uMouse) * 3.2);",
    "  color += vec3(0.10, 0.55, 0.68) * mouseGlow * 0.16 * uMouseStrength;",
    "",
    "  float vignette = smoothstep(1.45, 0.20, length(p));",
    "  color *= mix(0.55, 1.0, vignette);",
    "  color *= 1.0 - 0.25 * smoothstep(0.55, 1.0, uv.y);",
    "",
    "  float grain = hash(gl_FragCoord.xy + fract(uTime) * 123.0) - 0.5;",
    "  color += grain * 0.006;",
    "",
    "  gl_FragColor = vec4(color, 1.0);",
    "}",
  ].join("\n");

  var program = null;
  var uniforms = {};
  var running = false;
  var raf = 0;
  var lastFrame = 0;
  var startTime = 0;
  var width = 1;
  var height = 1;

  var pointer = {
    x: 0.72,
    y: 0.42,
    targetX: 0.72,
    targetY: 0.42,
    strength: 0,
    targetStrength: 0,
  };

  function compile(type, source) {
    var shader = gl.createShader(type);
    if (!shader) return null;
    gl.shaderSource(shader, source);
    gl.compileShader(shader);
    if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
      if (window.console && console.warn) {
        console.warn("Capture Button background shader:", gl.getShaderInfoLog(shader));
      }
      gl.deleteShader(shader);
      return null;
    }
    return shader;
  }

  function setup() {
    var vertex = compile(gl.VERTEX_SHADER, VERTEX);
    var fragment = compile(gl.FRAGMENT_SHADER, FRAGMENT);
    if (!vertex || !fragment) return false;

    program = gl.createProgram();
    if (!program) return false;
    gl.attachShader(program, vertex);
    gl.attachShader(program, fragment);
    gl.linkProgram(program);
    gl.deleteShader(vertex);
    gl.deleteShader(fragment);

    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
      if (window.console && console.warn) {
        console.warn("Capture Button background link:", gl.getProgramInfoLog(program));
      }
      return false;
    }

    gl.useProgram(program);

    var buffer = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);

    var position = gl.getAttribLocation(program, "aPos");
    gl.enableVertexAttribArray(position);
    gl.vertexAttribPointer(position, 2, gl.FLOAT, false, 0, 0);

    uniforms.res = gl.getUniformLocation(program, "uRes");
    uniforms.time = gl.getUniformLocation(program, "uTime");
    uniforms.mouse = gl.getUniformLocation(program, "uMouse");
    uniforms.mouseStrength = gl.getUniformLocation(program, "uMouseStrength");

    gl.disable(gl.DEPTH_TEST);
    gl.disable(gl.BLEND);
    return true;
  }

  function resize() {
    var cssWidth = Math.max(1, window.innerWidth);
    var cssHeight = Math.max(1, window.innerHeight);
    var dpr = Math.min(window.devicePixelRatio || 1, 2);
    var scale = Math.min(1, Math.sqrt(1600000 / (cssWidth * cssHeight * dpr * dpr))) * 0.9;
    scale = Math.max(0.45, Math.min(0.9, scale));
    width = Math.max(1, Math.round(cssWidth * dpr * scale));
    height = Math.max(1, Math.round(cssHeight * dpr * scale));
    canvas.width = width;
    canvas.height = height;
    gl.viewport(0, 0, width, height);
    if (uniforms.res) gl.uniform2f(uniforms.res, width, height);
    if (!running) draw(0);
  }

  function draw(time) {
    if (!program) return;

    pointer.x += (pointer.targetX - pointer.x) * 0.075;
    pointer.y += (pointer.targetY - pointer.y) * 0.075;
    if (pointer.targetStrength > 0) {
      pointer.strength += (pointer.targetStrength - pointer.strength) * 0.04;
    } else {
      pointer.strength *= 0.94;
    }

    gl.uniform1f(uniforms.time, time);
    gl.uniform2f(uniforms.mouse, pointer.x, pointer.y);
    gl.uniform1f(uniforms.mouseStrength, pointer.strength);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
  }

  function frame(now) {
    if (!running) return;
    raf = window.requestAnimationFrame(frame);
    if (document.hidden) return;
    if (now - lastFrame < 24) return;
    lastFrame = now;
    draw((now - startTime) / 1000);
  }

  function start() {
    if (running || reduceQuery.matches || saveData || document.hidden) return;
    running = true;
    startTime = window.performance.now() - 1000;
    lastFrame = 0;
    raf = window.requestAnimationFrame(frame);
  }

  function stop() {
    running = false;
    if (raf) window.cancelAnimationFrame(raf);
    raf = 0;
  }

  function setPointer(event) {
    if (reduceQuery.matches) return;
    if (event.pointerType && event.pointerType !== "mouse" && event.pointerType !== "pen") return;
    var rect = canvas.getBoundingClientRect();
    if (!rect.height) return;
    pointer.targetX = ((event.clientX - rect.left) * 2 - rect.width) / rect.height;
    pointer.targetY = (rect.height - (event.clientY - rect.top) * 2) / rect.height;
    pointer.targetStrength = 1;
  }

  function clearPointer() {
    pointer.targetStrength = 0;
  }

  function onMotionChange() {
    if (reduceQuery.matches) {
      stop();
      draw(0);
    } else {
      start();
    }
  }

  if (!setup()) {
    canvas.hidden = true;
    document.documentElement.classList.add("no-webgl");
    return;
  }

  resize();

  window.addEventListener("resize", resize, { passive: true });
  window.addEventListener("pointermove", setPointer, { passive: true });
  document.documentElement.addEventListener("pointerleave", clearPointer);
  window.addEventListener("blur", clearPointer);
  document.addEventListener("visibilitychange", function () {
    if (document.hidden) {
      stop();
    } else {
      start();
    }
  });
  canvas.addEventListener("webglcontextlost", function (event) {
    event.preventDefault();
    stop();
  });

  if (typeof reduceQuery.addEventListener === "function") {
    reduceQuery.addEventListener("change", onMotionChange);
  } else if (typeof reduceQuery.addListener === "function") {
    reduceQuery.addListener(onMotionChange);
  }

  if (reduceQuery.matches || saveData) {
    draw(0);
  } else {
    start();
  }
})();

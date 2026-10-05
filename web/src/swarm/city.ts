// The swarm as a city (M41): the "city" theme, beside the field (blocks).
// Every channel means one thing a pane reports, so it reads at a glance:
//
// - a block is a cluster; its rows are machines (projects, clustering by
//   machine), and lots go in pane order, so a pane keeps its address and
//   buildings move only on a regroup;
// - height is how long its command has run (log scale), and it keeps the
//   last command's height when that's done; what runs until stopped
//   (servers, logs, apps, editors) and what isn't a process (PRs, issues)
//   stands low and fixed;
// - a lit roof is a command still running (only things that finish);
// - the windows are its output: scrolling while it prints (faster for more
//   bytes), still when it printed lately, dark when it's quiet;
// - colour is kind (the field's palette); shape is lifecycle: a box
//   finishes, a drum runs until stopped (servers, logs, studio apps), a
//   hexagon is an agent, a pentagon an editor, a flat slab a pull request
//   or issue (a document, not a process);
// - a red roof: its last command failed; a beam: it needs you, in the
//   reason's colour, taller the longer it waits; greyed and dark: its host
//   isn't live; a marker over it: a teammate has it open (a cone while they
//   type).
//
// three.js loads only when the city is picked (the view imports this file
// lazily). Same hooks as the field: hover peeks, a click opens, a block's
// name dives, right-click is the pane's menu.

import * as THREE from "three";
import { OrbitControls } from "three/examples/jsm/controls/OrbitControls.js";
import type { WorkKind } from "../proto";
import type { FieldHooks, FieldPane, SwarmScene } from "./field";
import { KINDS } from "./model";

type Shape = "box" | "hex" | "pent" | "drum" | "slab";
const SHAPE: Record<WorkKind, Shape> = {
  shell: "box", build: "box", test: "box", agent: "hex", server: "drum", logs: "drum", app: "drum", editor: "pent", pr: "slab", issue: "slab", fountain: "slab",
};
/** Runs until stopped (or isn't a process at all): a fixed height, never a
 * lit roof. */
const LONG: Partial<Record<WorkKind, number>> = { server: 1.5, logs: 0.9, app: 1.8, editor: 0.7, pr: 0.35, issue: 0.35, fountain: 0.35 };

const STEP = 1.2;
const STREET = 4.4;
const ALLEY = 0.75;
const VOID = 0x06080d;
const reduce = typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;

/** Height for a command that has run `sec` seconds: a 10 s test and a 40
 * minute agent both read. */
export function heightFor(sec: number): number {
  return Math.min(13, 0.45 + 1.4 * Math.log2(1 + Math.max(0, sec) / 8));
}

/** A beam for something that has waited `sec` seconds: a short spike when
 * fresh, into the sky after an hour. */
export function beamFor(sec: number): number {
  return Math.min(70, 3 + 9 * Math.log2(1 + Math.max(0, sec) / 20));
}

/** How tall a pane stands now. */
export function standOf(p: FieldPane, now: number): number {
  const fixed = LONG[p.kind];
  if (fixed !== undefined) return fixed;
  if (p.started != null) {
    // Waiting on you, it stops where it was.
    const end = p.att?.since && p.att.since > p.started ? Math.min(now, p.att.since) : now;
    return heightFor((end - p.started) / 1000);
  }
  if (p.lastDur != null) return heightFor(p.lastDur / 1000);
  return 0.12;
}

function rateOf(bps: number): number {
  return bps > 0 ? Math.min(1, 0.15 + Math.log10(bps) / 4.2) : 0;
}

function hash(s: string): number {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
  return ((h >>> 0) % 10000) / 100;
}

interface Building extends FieldPane {
  shape: Shape;
  /** Stood on a lot already (a new one appears on its lot; others travel). */
  placed: boolean;
  slot: number;
  seed: number;
  x: number;
  z: number;
  tx: number;
  tz: number;
  fx: number;
  fz: number;
  h: number;
  scroll: number;
  block: Block | null;
}

interface Block {
  name: string;
  bs: Building[];
  subs: { name: string; z: number }[];
  w: number;
  d: number;
  x: number;
  z: number;
}

interface Beam {
  g: THREE.Group;
  core: THREE.Mesh<THREE.BufferGeometry, THREE.ShaderMaterial>;
  halo: THREE.Mesh<THREE.BufferGeometry, THREE.ShaderMaterial>;
  cap: THREE.Mesh;
  life: number;
  dying: boolean;
}

interface Person {
  name: string;
  g: THREE.Group;
  cone: THREE.Mesh;
  tag: THREE.Sprite;
  tags: { watching: THREE.Texture; driving: THREE.Texture; typing: THREE.Texture };
  key: string | null;
  driving: boolean;
  typing: boolean;
  from: THREE.Vector3;
  t: number;
  k: number;
}

const towerVert = /* glsl */ `
  attribute vec3 aCol; attribute vec4 aA; attribute vec4 aB;
  varying vec3 vP; varying vec3 vN; varying vec3 vC; varying vec4 vA; varying vec4 vB; varying vec3 vL; varying float vTop;
  void main(){
    vec4 w = modelMatrix*instanceMatrix*vec4(position,1.);
    vP = w.xyz; vL = position; vTop = instanceMatrix[3][1] + instanceMatrix[1][1];
    vN = normalize(mat3(modelMatrix*instanceMatrix)*normal);
    vC = aCol; vA = aA; vB = aB;
    gl_Position = projectionMatrix*viewMatrix*w;
  }`;

// aA: rate (0 silent), glow (how lately it printed), seed, stale.
// aB: running, last failed, unused, scroll (accumulated, so text freezes
// where it stopped).
const towerFrag = /* glsl */ `
  uniform vec3 uFog; uniform float uFogNear; uniform float uFogFar;
  varying vec3 vP; varying vec3 vN; varying vec3 vC; varying vec4 vA; varying vec4 vB; varying vec3 vL; varying float vTop;
  float h21(vec2 p){ p = fract(p*vec2(123.34,456.21)); p += dot(p,p+45.32); return fract(p.x*p.y); }
  void main(){
    float rate = vA.x, glow = vA.y, s = vA.z, stale = vA.w;
    float running = vB.x, failed = vB.y, scroll = vB.w;
    vec3 n = normalize(vN);
    float shade = .55 + .45*max(dot(n, normalize(vec3(.4,.9,.3))),0.);
    vec3 base = vec3(.08,.09,.115)*shade;
    vec3 kc = stale > .5 ? vec3(.25) : vC;
    vec3 col = base;
    if (running > .5 && n.y > .5) {
      col = kc*.85 + .08;
    } else if (running > .5 && vP.y > vTop - .14) {
      col = kc*1.1 + .1;
    } else if (n.y > .5) {
      float r = max(abs(vL.x),abs(vL.z));
      col = base*1.2 + kc*.12*smoothstep(.36,.5,r);
      if (failed > .5) col = mix(col, vec3(1.,.33,.41), .85);
    } else {
      float u = (vP.x + vP.z)*7.;
      float v = vP.y*9. + scroll;
      float row = floor(v);
      float lenRow = h21(vec2(row, s));
      float cellOn = step(h21(vec2(floor(u), row + s*3.)), .8);
      float on = cellOn * step(fract(u/9.), lenRow) * step(.3, fract(v)) * step(.18, fract(u));
      on *= step(h21(vec2(row*1.7, s)), .3 + .6*max(rate, glow*.6));
      float lit = rate > 0. ? .45 + rate : glow*.7;
      col += kc * on * (lit + .035);
    }
    if (stale > .5) col *= .45;
    gl_FragColor = vec4(mix(col, uFog, smoothstep(uFogNear, uFogFar, length(vP - cameraPosition))), 1.);
  }`;

const MONO = `"JetBrains Mono", ui-monospace, monospace`;
const DISPLAY = `"Big Shoulders Display", "Arial Narrow", sans-serif`;

function textTexture(lines: [string, string, string, number, number][], w: number, h: number): THREE.CanvasTexture {
  const c = document.createElement("canvas");
  c.width = w;
  c.height = h;
  const x = c.getContext("2d")!;
  for (const [text, font, color, px, py] of lines) {
    x.font = font;
    x.fillStyle = color;
    x.fillText(text, px, py);
  }
  const t = new THREE.CanvasTexture(c);
  t.anisotropy = 8;
  t.colorSpace = THREE.SRGBColorSpace;
  return t;
}

function shapeGeometry(s: Shape): THREE.BufferGeometry {
  const g =
    s === "box"
      ? new THREE.BoxGeometry(0.78, 1, 0.78)
      : s === "hex"
        ? new THREE.CylinderGeometry(0.46, 0.46, 1, 6)
        : s === "pent"
          ? new THREE.CylinderGeometry(0.47, 0.47, 1, 5)
          : s === "slab"
            ? new THREE.BoxGeometry(0.98, 1, 0.62)
            : new THREE.CylinderGeometry(0.47, 0.47, 1, 20);
  g.translate(0, 0.5, 0);
  return g;
}

function additive(frag: string, uniforms: Record<string, THREE.IUniform>, side: THREE.Side = THREE.FrontSide): THREE.ShaderMaterial {
  return new THREE.ShaderMaterial({
    uniforms,
    transparent: true,
    depthWrite: false,
    side,
    blending: THREE.AdditiveBlending,
    vertexShader: `varying vec3 vPos; varying vec2 vUv; void main(){ vPos = position; vUv = uv; gl_Position = projectionMatrix*modelViewMatrix*vec4(position,1.); }`,
    fragmentShader: frag,
  });
}

export class City implements SwarmScene {
  private renderer: THREE.WebGLRenderer;
  private scene = new THREE.Scene();
  private camera = new THREE.PerspectiveCamera(42, 1, 0.1, 6000);
  private controls: OrbitControls;
  private uni = {
    uTime: { value: 0 },
    uFog: { value: new THREE.Color(VOID) },
    uFogNear: { value: 45 },
    uFogFar: { value: 200 },
  };
  private towerMat: THREE.ShaderMaterial;
  private meshes = new Map<Shape, { mesh: THREE.InstancedMesh; list: Building[] }>();
  private bs = new Map<string, Building>();
  private blocks: Block[] = [];
  private blockLayer = new THREE.Group();
  private blockSig = "";
  private sigAt = 0;
  private beams = new Map<string, Beam>();
  private people = new Map<string, Person>();
  private beamGeo: THREE.BufferGeometry;
  private haloGeo: THREE.BufferGeometry;
  private coneGeo: THREE.BufferGeometry;
  private coneMat: THREE.ShaderMaterial;
  private size = { w: 40, d: 40 };
  private raf = 0;
  private last = 0;
  private moveT0 = -1e9;
  private fly: { p0: THREE.Vector3; t0: THREE.Vector3; p1: THREE.Vector3; t1: THREE.Vector3; s: number; ms: number } | null = null;
  /** Someone orbited, zoomed or dived: stop fitting by ourselves. */
  private userMoved = false;
  private fitted = false;
  private fontsReady = false;
  private down: { x: number; y: number } | null = null;
  private ray = new THREE.Raycaster();
  private measuring: { gaps: number[]; work: number[] } | null = null;
  private off: (() => void)[] = [];

  constructor(
    private cv: HTMLCanvasElement,
    private hooks: FieldHooks,
  ) {
    this.renderer = new THREE.WebGLRenderer({ canvas: cv, antialias: true });
    this.renderer.setPixelRatio(Math.min(devicePixelRatio || 1, 2));
    this.renderer.setClearColor(VOID, 1);
    this.controls = new OrbitControls(this.camera, cv);
    this.controls.enableDamping = true;
    this.controls.dampingFactor = 0.08;
    this.controls.maxPolarAngle = 1.36;
    this.controls.minDistance = 4;
    this.controls.maxDistance = 3000;
    this.controls.addEventListener("start", () => {
      this.userMoved = true;
      this.fly = null;
    });

    const ground = new THREE.Mesh(
      new THREE.PlaneGeometry(2000, 2000),
      new THREE.ShaderMaterial({
        uniforms: this.uni,
        vertexShader: `varying vec3 vP; void main(){ vec4 w = modelMatrix*vec4(position,1.); vP=w.xyz; gl_Position=projectionMatrix*viewMatrix*w; }`,
        fragmentShader: `uniform vec3 uFog; uniform float uFogNear; uniform float uFogFar; varying vec3 vP;
          float line(float v, float w){ float f = abs(fract(v)-.5); return smoothstep(.5-w,.5,f); }
          void main(){
            vec3 c = vec3(.035,.045,.065) + vec3(.045,.06,.085)*max(line(vP.x/2.,.02), line(vP.z/2.,.02));
            gl_FragColor = vec4(mix(c, uFog, smoothstep(uFogNear, uFogFar*.95, length(vP - cameraPosition))), 1.);
          }`,
      }),
    );
    ground.rotation.x = -Math.PI / 2;
    ground.name = "ground";
    this.scene.add(ground, this.blockLayer);

    this.towerMat = new THREE.ShaderMaterial({ uniforms: this.uni, vertexShader: towerVert, fragmentShader: towerFrag });
    this.beamGeo = new THREE.CylinderGeometry(0.06, 0.06, 1, 6, 1, true).translate(0, 0.5, 0);
    this.haloGeo = new THREE.CylinderGeometry(0.3, 0.3, 1, 12, 1, true).translate(0, 0.5, 0);
    this.coneGeo = new THREE.ConeGeometry(1, 1, 24, 1, true).translate(0, -0.5, 0);
    this.coneMat = additive(`varying vec3 vPos; void main(){ float y = -vPos.y; gl_FragColor = vec4(vec3(.75,.85,1.)*.17*(1.-y*.7),1.); }`, {}, THREE.DoubleSide);

    this.listen();
    // Block names are drawn into textures: draw them again in the swarm's
    // faces once those load.
    void document.fonts?.load(`800 112px "Big Shoulders Display"`).then(() => document.fonts.load(`500 34px "JetBrains Mono"`)).then(() => {
      this.fontsReady = true;
      this.blockSig = "";
    });
  }

  // ---- data

  set(panes: FieldPane[]) {
    const now = Date.now();
    const seen = new Set<string>();
    let membership = false;
    for (const p of panes) {
      seen.add(p.key);
      const shape = SHAPE[p.kind] ?? "box";
      let b = this.bs.get(p.key);
      if (!b || b.shape !== shape) {
        b = { ...p, shape, placed: false, slot: -1, seed: hash(p.key), x: 0, z: 0, tx: 0, tz: 0, fx: 0, fz: 0, h: standOf(p, now), scroll: hash(p.key + "s"), block: null };
        this.bs.set(p.key, b);
        membership = true;
      } else {
        if (b.group !== p.group || b.sub !== p.sub) membership = true;
        Object.assign(b, p);
      }
    }
    for (const k of [...this.bs.keys()]) {
      if (!seen.has(k)) {
        this.bs.delete(k);
        membership = true;
      }
    }
    if (membership) this.rebuild();
    this.syncBeams();
    this.syncPeople();
  }

  /** The grouping changed: everyone moves to their new lot. */
  regroup() {
    this.rebuild();
    this.userMoved = false;
    this.fit();
  }

  private rebuild() {
    for (const b of this.bs.values()) {
      b.fx = b.x;
      b.fz = b.z;
    }
    this.layout();
    // A new building appears on its lot; the rest travel to theirs.
    for (const b of this.bs.values()) {
      if (!b.placed) {
        b.placed = true;
        b.x = b.fx = b.tx;
        b.z = b.fz = b.tz;
      }
    }
    this.moveT0 = performance.now();
    this.allocate();
    this.blockSig = "";
    if (!this.userMoved) this.fit();
  }

  /** One instanced mesh per shape, sized to fit. */
  private allocate() {
    for (const shape of ["box", "hex", "pent", "drum", "slab"] as Shape[]) {
      const list = [...this.bs.values()].filter((b) => b.shape === shape);
      let m = this.meshes.get(shape);
      if (!m || m.mesh.instanceMatrix.count < list.length) {
        if (m) {
          this.scene.remove(m.mesh);
          m.mesh.geometry.dispose();
          m.mesh.dispose();
        }
        const cap = Math.max(16, Math.ceil(list.length * 1.5));
        const geo = shapeGeometry(shape);
        geo.setAttribute("aCol", new THREE.InstancedBufferAttribute(new Float32Array(cap * 3), 3));
        geo.setAttribute("aA", new THREE.InstancedBufferAttribute(new Float32Array(cap * 4), 4));
        geo.setAttribute("aB", new THREE.InstancedBufferAttribute(new Float32Array(cap * 4), 4));
        const mesh = new THREE.InstancedMesh(geo, this.towerMat, cap);
        mesh.instanceMatrix.setUsage(THREE.DynamicDrawUsage);
        mesh.frustumCulled = false;
        this.scene.add(mesh);
        m = { mesh, list };
        this.meshes.set(shape, m);
      }
      m.list = list;
      m.mesh.count = list.length;
      list.forEach((b, i) => (b.slot = i));
    }
  }

  // ---- layout: stable addresses

  private layout() {
    const groups = new Map<string, Building[]>();
    for (const b of this.bs.values()) groups.set(b.group, [...(groups.get(b.group) ?? []), b]);
    const list: Block[] = [...groups.entries()]
      .sort((a, b) => a[0].localeCompare(b[0]))
      .map(([name, bs]) => {
        const cols = Math.max(4, Math.ceil(Math.sqrt(bs.length * 1.6)));
        const sorted = [...bs].sort((a, b) => (a.sub ?? a.where).localeCompare(b.sub ?? b.where) || (a.id ?? 0) - (b.id ?? 0) || a.key.localeCompare(b.key));
        const subs: { name: string; z: number }[] = [];
        let c = 0;
        let z = 0;
        let cur: string | null = null;
        const lots = new Map<Building, { x: number; z: number }>();
        for (const b of sorted) {
          const s = b.sub ?? b.where;
          if (s !== cur) {
            if (cur !== null) z += STEP + ALLEY;
            c = 0;
            cur = s;
            subs.push({ name: s, z });
          } else if (c === cols) {
            c = 0;
            z += STEP;
          }
          lots.set(b, { x: 0.5 + STEP / 2 + c * STEP, z: ALLEY + 0.2 + STEP / 2 + z });
          c++;
        }
        const blk: Block = { name, bs: sorted, subs, w: cols * STEP + 1, d: z + STEP + ALLEY + 0.6, x: 0, z: 0 };
        for (const [b, l] of lots) {
          b.block = blk;
          b.tx = l.x;
          b.tz = l.z;
        }
        return blk;
      });
    // Shelf-pack into a roughly square city.
    const area = list.reduce((s, b) => s + (b.w + STREET) * (b.d + STREET), 0);
    const maxW = Math.max(Math.max(0, ...list.map((b) => b.w)) + STREET, Math.sqrt(area) * 1.2);
    let x = 0;
    let z = 0;
    let rowD = 0;
    let w = 0;
    for (const b of list) {
      if (x > 0 && x + b.w > maxW) {
        x = 0;
        z += rowD + STREET;
        rowD = 0;
      }
      b.x = x;
      b.z = z;
      x += b.w + STREET;
      rowD = Math.max(rowD, b.d);
      w = Math.max(w, x - STREET);
    }
    const d = z + rowD;
    for (const b of list) {
      b.x -= w / 2;
      b.z -= d / 2;
      for (const bl of b.bs) {
        bl.tx += b.x;
        bl.tz += b.z;
      }
    }
    this.blocks = list;
    this.size = { w: Math.max(w, 8), d: Math.max(d, 8) };
  }

  private needOf(b: Block) {
    return b.bs.filter((x) => x.att).length;
  }
  private failedIn(b: Block) {
    return b.bs.some((x) => x.att && x.lastExit != null && x.lastExit !== 0 && x.started == null);
  }

  /** Plates, walls and names; drawn again when what they say changes. */
  private drawBlocks() {
    for (const ch of [...this.blockLayer.children]) {
      this.blockLayer.remove(ch);
      const m = ch as THREE.Mesh<THREE.BufferGeometry, THREE.MeshBasicMaterial>;
      m.geometry?.dispose();
      m.material?.map?.dispose();
      m.material?.dispose();
    }
    for (const b of this.blocks) {
      const hot = this.failedIn(b);
      const need = this.needOf(b);
      const cx = b.x + b.w / 2;
      const cz = b.z + b.d / 2;
      const plate = new THREE.Mesh(new THREE.BoxGeometry(b.w, 0.12, b.d), new THREE.MeshBasicMaterial({ color: 0x0b0f17 }));
      plate.position.set(cx, 0.06, cz);
      plate.userData.block = b.name;
      this.blockLayer.add(plate);
      const H = hot ? 1.1 : 0.6;
      for (const [len, x, z, ry] of [
        [b.w, cx, b.z, 0],
        [b.w, cx, b.z + b.d, 0],
        [b.d, b.x, cz, Math.PI / 2],
        [b.d, b.x + b.w, cz, Math.PI / 2],
      ]) {
        const wall = new THREE.Mesh(
          new THREE.PlaneGeometry(len, H),
          additive(
            `uniform vec3 uC; uniform float uA; varying vec2 vUv; void main(){ float y = vUv.y; float a = uA*(1.-y)*(1.-y) + uA*1.4*smoothstep(.9,1.,y); gl_FragColor = vec4(uC*a, 1.); }`,
            { uC: { value: new THREE.Color(hot ? 0xff3050 : 0x5f7898) }, uA: { value: hot ? 0.5 : 0.2 } },
            THREE.DoubleSide,
          ),
        );
        wall.position.set(x, H / 2 + 0.12, z);
        wall.rotation.y = ry;
        this.blockLayer.add(wall);
      }
      const running = b.bs.filter((x) => x.started != null && LONG[x.kind] === undefined).length;
      const meta = `${b.bs.length} pane${b.bs.length === 1 ? "" : "s"} · ${running} running`;
      const lines: [string, string, string, number, number][] = [
        [b.name.toUpperCase(), `800 112px ${DISPLAY}`, "#e6ebf4", 8, 112],
        [meta, `500 34px ${MONO}`, "#7c8698", 8, 170],
      ];
      if (need) lines.push([`· ${need} need you`, `500 34px ${MONO}`, "#ff5468", 8 + meta.length * 20.6 + 20, 170]);
      const lw = Math.min(Math.max(b.w, 6), 9);
      const lh = lw / (1024 / 200);
      const label = new THREE.Mesh(new THREE.PlaneGeometry(lw, lh), new THREE.MeshBasicMaterial({ map: textTexture(lines, 1024, 200), transparent: true, depthWrite: false }));
      label.rotation.x = -Math.PI / 2;
      label.position.set(b.x + lw / 2, 0.02, b.z + b.d + lh / 2 + 0.25);
      label.userData.block = b.name;
      this.blockLayer.add(label);
      for (const s of b.subs) {
        const sw = 4;
        const stale = b.bs.every((x) => (x.sub ?? x.where) !== s.name || x.stale);
        const sl = new THREE.Mesh(
          new THREE.PlaneGeometry(sw, sw / 8),
          new THREE.MeshBasicMaterial({ map: textTexture([[s.name, `500 40px ${MONO}`, stale ? "#55606f" : "#8a95a8", 4, 44]], 512, 64), transparent: true, depthWrite: false }),
        );
        sl.rotation.x = -Math.PI / 2;
        sl.position.set(b.x + 0.5 + sw / 2, 0.125, b.z + s.z + ALLEY / 2 + 0.12);
        this.blockLayer.add(sl);
      }
    }
  }

  private signature() {
    return (
      (this.fontsReady ? "f" : "") +
      this.blocks
        .map((b) => `${b.name}:${b.bs.length}:${this.needOf(b)}:${this.failedIn(b) ? 1 : 0}:${b.bs.filter((x) => x.started != null && LONG[x.kind] === undefined).length}`)
        .join("|")
    );
  }

  // ---- beams and people

  private syncBeams() {
    for (const b of this.bs.values()) {
      if (!b.att || this.beams.has(b.key)) continue;
      const col = new THREE.Color(b.att.col[0] / 255, b.att.col[1] / 255, b.att.col[2] / 255);
      const frag = `uniform vec3 uC; uniform float uA; varying vec3 vPos; void main(){ gl_FragColor = vec4(uC*uA*(1.-vPos.y*.85),1.); }`;
      const core = new THREE.Mesh(this.beamGeo, additive(frag, { uC: { value: col }, uA: { value: 0 } }));
      const halo = new THREE.Mesh(this.haloGeo, additive(frag, { uC: { value: col }, uA: { value: 0 } }));
      const cap = new THREE.Mesh(new THREE.SphereGeometry(0.2, 12, 8), new THREE.MeshBasicMaterial({ color: col }));
      const g = new THREE.Group();
      g.add(core, halo, cap);
      this.scene.add(g);
      this.beams.set(b.key, { g, core, halo, cap, life: 0, dying: false });
    }
    for (const [k, beam] of this.beams) if (!this.bs.get(k)?.att) beam.dying = true;
  }

  private syncPeople() {
    const want = new Map<string, { key: string; driving: boolean; typing: boolean }>();
    const rank = (p: { driving: boolean; typing: boolean }) => +p.typing * 2 + +p.driving;
    for (const b of this.bs.values()) {
      for (const p of b.people ?? []) {
        const had = want.get(p.name);
        if (!had || rank(p) > rank(had)) want.set(p.name, { key: b.key, driving: p.driving, typing: p.typing });
      }
    }
    for (const [name, m] of this.people) {
      if (want.has(name)) continue;
      this.scene.remove(m.g);
      this.people.delete(name);
    }
    let k = this.people.size;
    for (const [name, w] of want) {
      let m = this.people.get(name);
      if (!m) {
        const g = new THREE.Group();
        const col = new THREE.Color().setHSL((hash(name) / 100) % 1, 0.55, 0.75);
        const body = new THREE.Mesh(new THREE.OctahedronGeometry(0.3), new THREE.MeshBasicMaterial({ color: col }));
        const ring = new THREE.Mesh(new THREE.TorusGeometry(0.5, 0.03, 6, 32), new THREE.MeshBasicMaterial({ color: col }));
        ring.rotation.x = Math.PI / 2;
        const cone = new THREE.Mesh(this.coneGeo, this.coneMat);
        const tags = {
          watching: textTexture([[`${name} · watching`, `600 30px ${MONO}`, "#e6ebf4", 14, 43]], 384, 64),
          driving: textTexture([[`${name} · driving`, `600 30px ${MONO}`, "#e6ebf4", 14, 43]], 384, 64),
          typing: textTexture([[`${name} · typing`, `600 30px ${MONO}`, "#e6ebf4", 14, 43]], 384, 64),
        };
        const tag = new THREE.Sprite(new THREE.SpriteMaterial({ map: tags.watching, depthTest: false, transparent: true }));
        tag.scale.set(4, 0.67, 1);
        tag.position.y = 0.95;
        g.add(body, ring, cone, tag);
        g.userData.body = body;
        const at = this.bs.get(w.key);
        g.position.set(at?.x ?? 0, 14, at?.z ?? 0);
        this.scene.add(g);
        m = { name, g, cone, tag, tags, key: null, typing: false, driving: false, from: g.position.clone(), t: 1, k: k++ };
        this.people.set(name, m);
      }
      if (m.key !== w.key) {
        m.key = w.key;
        m.from.copy(m.g.position);
        m.t = 0;
      }
      if (m.driving !== w.driving || m.typing !== w.typing) {
        m.driving = w.driving;
        m.typing = w.typing;
        (m.tag.material as THREE.SpriteMaterial).map = w.typing ? m.tags.typing : w.driving ? m.tags.driving : m.tags.watching;
      }
    }
  }

  // ---- camera

  /** The free part of the canvas (what the bar and the rail leave), in
   * pixels: the camera centres on it and fits into it. */
  private free() {
    const w = this.cv.clientWidth || 1;
    const h = this.cv.clientHeight || 1;
    const rw = this.hooks.railW();
    const rh = this.hooks.railH();
    const top = Math.min(h * 0.4, Math.max(0, this.hooks.top()));
    return { w, h, rw, rh, top, fw: Math.max(80, w - rw), fh: Math.max(80, h - rh - top) };
  }

  /** Looking down at about 50 degrees, as close as lets the whole city
   * (and the first storeys of its towers) fit in the free part of the
   * screen: corners projected, distance halved until they do. */
  private homePose(): [THREE.Vector3, THREE.Vector3] {
    const f = this.free();
    const target = new THREE.Vector3(0, 0, 0);
    const dir = new THREE.Vector3(0.12, Math.tan(THREE.MathUtils.degToRad(50)), 1).normalize();
    // The free part, in the canvas's clip space (y up), with a margin.
    const x1 = -1 + (2 * f.fw) / f.w;
    const y0 = -1 + (2 * f.rh) / f.h;
    const y1 = 1 - (2 * f.top) / f.h;
    const mx = 0.06 * (x1 + 1);
    const my = 0.06 * (y1 - y0);
    const hw = this.size.w / 2;
    const hd = this.size.d / 2;
    const corners: THREE.Vector3[] = [];
    for (const x of [-hw, hw]) for (const z of [-hd, hd]) for (const y of [0, 3]) corners.push(new THREE.Vector3(x, y, z));
    const cam = this.camera.clone();
    const fits = (d: number) => {
      cam.position.copy(dir).multiplyScalar(d).add(target);
      cam.lookAt(target);
      cam.updateMatrixWorld();
      return corners.every((c) => {
        const v = c.clone().project(cam);
        return v.z < 1 && v.x >= -1 + mx && v.x <= x1 - mx && v.y >= y0 + my && v.y <= y1 - my;
      });
    };
    let lo = 2;
    let hi = 3000;
    for (let i = 0; i < 24; i++) {
      const mid = (lo + hi) / 2;
      if (fits(mid)) hi = mid;
      else lo = mid;
    }
    return [dir.multiplyScalar(hi).add(target), target];
  }

  private flyTo(p1: THREE.Vector3, t1: THREE.Vector3, ms = 1400) {
    this.fly = { p0: this.camera.position.clone(), t0: this.controls.target.clone(), p1, t1, s: performance.now(), ms: reduce ? 1 : ms };
  }

  private fit() {
    const [p, t] = this.homePose();
    if (!this.fitted) {
      this.fitted = true;
      this.camera.position.copy(p);
      this.controls.target.copy(t);
      return;
    }
    this.flyTo(p, t);
  }

  fitAll() {
    this.userMoved = false;
    this.fit();
  }

  /** Fly to a block (clicking its name or plate). */
  dive(name: string) {
    const b = this.blocks.find((x) => x.name === name);
    if (!b) return;
    this.userMoved = true;
    const cx = b.x + b.w / 2;
    const cz = b.z + b.d / 2;
    const r = Math.max(b.w, b.d);
    this.flyTo(new THREE.Vector3(cx + r * 0.1, r * 0.9 + 4, cz + r * 1.1 + 4), new THREE.Vector3(cx, 0, cz));
  }

  diveTo(key: string) {
    const b = this.bs.get(key);
    if (!b) return;
    this.userMoved = true;
    const h = standOf(b, Date.now());
    const dir = new THREE.Vector3(this.camera.position.x - b.tx, 0, this.camera.position.z - b.tz);
    if (dir.lengthSq() < 1e-6) dir.set(0, 0, 1);
    dir.normalize();
    this.flyTo(new THREE.Vector3(b.tx + dir.x * 7, Math.max(4, h * 0.9 + 3.5), b.tz + dir.z * 7), new THREE.Vector3(b.tx, h * 0.55, b.tz), 1500);
  }

  /** Where the camera is looking (tests check a dive went there). */
  get target(): { x: number; z: number } {
    const t = this.fly ? this.fly.t1 : this.controls.target;
    return { x: t.x, z: t.z };
  }
  /** Where a pane stands on its lot (tests compare it with the target). */
  lotOf(key: string): { x: number; z: number } | null {
    const b = this.bs.get(key);
    return b ? { x: b.tx, z: b.tz } : null;
  }

  /** Panes with a beam up (tests). */
  get beamKeys(): string[] {
    return [...this.beams].filter(([, b]) => !b.dying).map(([k]) => k);
  }
  /** How tall a pane stands now (tests). */
  heightOf(key: string): number | null {
    const b = this.bs.get(key);
    return b ? standOf(b, Date.now()) : null;
  }

  get clusters(): { name: string; n: number; need: number }[] {
    return this.blocks.map((b) => ({ name: b.name, n: b.bs.length, need: this.needOf(b) }));
  }

  screenOf(key: string): { x: number; y: number } | null {
    const b = this.bs.get(key);
    if (!b) return null;
    const v = new THREE.Vector3(b.x, 0.12 + Math.max(0.1, b.h * 0.5), b.z).project(this.camera);
    if (v.z > 1) return null;
    const r = this.cv.getBoundingClientRect();
    return { x: r.left + ((v.x + 1) / 2) * r.width, y: r.top + ((1 - v.y) / 2) * r.height };
  }

  resize() {
    if (!this.cv.clientWidth || !this.cv.clientHeight) return;
    const { w, h, rw, rh, top } = this.free();
    this.renderer.setPixelRatio(Math.min(devicePixelRatio || 1, 2));
    this.renderer.setSize(w, h, false);
    // Centre the view on what the rail and the bar leave free: a larger
    // virtual frame, of which the canvas is the part offset by the rail
    // (right) and the strip (bottom), so the frame's centre is the free
    // part's centre.
    this.camera.aspect = (w + rw) / (h + rh + top);
    this.camera.setViewOffset(w + rw, h + rh + top, rw, rh, w, h);
    this.camera.updateProjectionMatrix();
    if (!this.userMoved && this.fitted) {
      const [p, t] = this.homePose();
      this.camera.position.copy(p);
      this.controls.target.copy(t);
    }
  }

  // ---- input

  private listen() {
    const cv = this.cv;
    const on = <K extends keyof HTMLElementEventMap>(type: K, fn: (e: HTMLElementEventMap[K]) => void) => {
      cv.addEventListener(type, fn as EventListener);
      this.off.push(() => cv.removeEventListener(type, fn as EventListener));
    };
    on("pointerdown", (e) => {
      this.down = { x: e.clientX, y: e.clientY };
      this.fly = null;
    });
    on("pointermove", (e) => {
      if (e.buttons || e.pointerType !== "mouse") return this.hooks.hover(null, 0, 0);
      const b = this.pick(e);
      cv.style.cursor = b || this.blockAt(e) ? "pointer" : "";
      const r = cv.getBoundingClientRect();
      this.hooks.hover(b?.key ?? null, e.clientX - r.left, e.clientY - r.top);
    });
    on("pointerleave", () => this.hooks.hover(null, 0, 0));
    on("pointerup", (e) => {
      const d = this.down;
      this.down = null;
      if (!d || Math.hypot(e.clientX - d.x, e.clientY - d.y) > 5 || e.button !== 0) return;
      const b = this.pick(e);
      if (b) return this.hooks.open(b.key);
      const name = this.blockAt(e);
      if (name) this.dive(name);
    });
    on("contextmenu", (e) => {
      const b = this.pick(e);
      if (b && this.hooks.menu) {
        this.hooks.hover(null, 0, 0);
        this.hooks.menu(b.key, e);
      } else e.preventDefault();
    });
  }

  private aim(e: MouseEvent) {
    const r = this.cv.getBoundingClientRect();
    this.ray.setFromCamera(new THREE.Vector2(((e.clientX - r.left) / r.width) * 2 - 1, -((e.clientY - r.top) / r.height) * 2 + 1), this.camera);
  }

  private pick(e: MouseEvent): Building | null {
    this.aim(e);
    // An InstancedMesh keeps the bounding sphere of its first raycast, but
    // buildings move and grow every frame: a stale one misses them all.
    for (const m of this.meshes.values()) m.mesh.computeBoundingSphere();
    const hits = this.ray.intersectObjects(
      [...this.meshes.values()].map((m) => m.mesh),
      false,
    );
    for (const h of hits) {
      const m = [...this.meshes.values()].find((x) => x.mesh === h.object);
      const b = m && h.instanceId !== undefined ? m.list[h.instanceId] : undefined;
      if (b) return b;
    }
    return null;
  }

  private blockAt(e: MouseEvent): string | null {
    this.aim(e);
    const h = this.ray.intersectObjects(this.blockLayer.children, false).find((x) => x.object.userData.block);
    return (h?.object.userData.block as string | undefined) ?? null;
  }

  // ---- loop

  start() {
    this.resize();
    this.last = performance.now();
    const M = new THREE.Matrix4();
    const Q = new THREE.Quaternion();
    const S = new THREE.Vector3();
    const P = new THREE.Vector3();
    const down = new THREE.Vector3(0, -1, 0);
    const ease = (t: number) => (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2);
    const frame = (t: number) => {
      this.raf = requestAnimationFrame(frame);
      const dt = Math.min(0.1, (t - this.last) / 1000);
      this.last = t;
      const t0 = performance.now();
      const now = Date.now();
      this.uni.uTime.value = t / 1000;
      // What the blocks say, looked at once a second.
      if (t - this.sigAt > 1000 || !this.blockSig) {
        this.sigAt = t;
        const sig = this.signature();
        if (sig !== this.blockSig) {
          this.blockSig = sig;
          this.drawBlocks();
        }
      }
      const mt = Math.min(1, (t - this.moveT0) / 1300);
      for (const m of this.meshes.values()) {
        const aCol = m.mesh.geometry.getAttribute("aCol") as THREE.InstancedBufferAttribute;
        const aA = m.mesh.geometry.getAttribute("aA") as THREE.InstancedBufferAttribute;
        const aB = m.mesh.geometry.getAttribute("aB") as THREE.InstancedBufferAttribute;
        for (const b of m.list) {
          if (mt < 1) {
            const k = ease(Math.min(1, Math.max(0, mt * 1.35 - ((b.seed * 7) % 17) / 17 * 0.35)));
            b.x = b.fx + (b.tx - b.fx) * k;
            b.z = b.fz + (b.tz - b.fz) * k;
          } else {
            b.x = b.tx;
            b.z = b.tz;
          }
          const th = standOf(b, now);
          b.h += (th - b.h) * (reduce ? 1 : th < b.h ? 0.12 : 0.25);
          const rate = b.stale ? 0 : rateOf(b.bps ?? 0);
          if (!reduce) b.scroll += (rate > 0 ? 1.2 + rate * 11 : 0) * dt;
          P.set(b.x, 0.12, b.z);
          S.set(1, Math.max(0.04, b.h), 1);
          M.compose(P, Q, S);
          m.mesh.setMatrixAt(b.slot, M);
          const c = KINDS[b.kind] ?? KINDS.shell;
          aCol.setXYZ(b.slot, c[0] / 255, c[1] / 255, c[2] / 255);
          const glow = b.stale || !b.lastOut ? 0 : Math.exp(-(now - b.lastOut) / 75000);
          aA.setXYZW(b.slot, rate, glow, b.seed, b.stale ? 1 : 0);
          const running = b.started != null && LONG[b.kind] === undefined && !b.stale ? 1 : 0;
          aB.setXYZW(b.slot, running, !running && b.lastExit != null && b.lastExit !== 0 ? 1 : 0, 0, b.scroll);
        }
        m.mesh.instanceMatrix.needsUpdate = true;
        aCol.needsUpdate = aA.needsUpdate = aB.needsUpdate = true;
      }
      for (const [k, beam] of this.beams) {
        const b = this.bs.get(k);
        if (beam.dying || !b) {
          beam.life -= 0.05;
          if (beam.life <= 0) {
            this.scene.remove(beam.g);
            beam.core.material.dispose();
            beam.halo.material.dispose();
            this.beams.delete(k);
            continue;
          }
        } else beam.life = Math.min(1, beam.life + 0.03);
        const pulse = reduce ? 1 : 0.88 + 0.12 * Math.sin(t / 350 + (b?.seed ?? 0));
        const a = Math.max(0, beam.life) * pulse;
        beam.core.material.uniforms.uA.value = 2 * a;
        beam.halo.material.uniforms.uA.value = 0.25 * a;
        if (!b) continue;
        const len = beamFor(b.att?.since ? (now - b.att.since) / 1000 : 0) * Math.max(0.01, beam.life);
        beam.g.position.set(b.x, 0.12 + b.h, b.z);
        beam.core.scale.y = beam.halo.scale.y = len;
        beam.cap.position.y = len;
      }
      for (const m of this.people.values()) {
        const b = m.key ? this.bs.get(m.key) : undefined;
        if (!b) continue;
        const to = new THREE.Vector3(b.x - 1 + (m.k % 3) * 0.5, b.h + 3.8 + (m.k % 3) * 0.6, b.z + 1.3);
        m.t = Math.min(1, m.t + (reduce ? 1 : 0.008));
        const k = ease(m.t);
        m.g.position.lerpVectors(m.from, to, k);
        m.g.position.y += Math.sin(k * Math.PI) * 3 + (reduce ? 0 : Math.sin(t / 700 + m.k) * 0.12);
        const dx = b.x - m.g.position.x;
        const dz = b.z - m.g.position.z;
        const dy = m.g.position.y - (b.h + 0.12);
        m.cone.scale.set(0.85, Math.hypot(dx, dy, dz), 0.85);
        m.cone.quaternion.setFromUnitVectors(down, new THREE.Vector3(dx, -dy, dz).normalize());
        m.cone.visible = m.typing && m.t > 0.85;
        (m.g.userData.body as THREE.Object3D).rotation.y = t / 900;
      }
      if (this.fly) {
        const f = Math.min(1, (t - this.fly.s) / this.fly.ms);
        const k = ease(f);
        this.camera.position.lerpVectors(this.fly.p0, this.fly.p1, k);
        this.controls.target.lerpVectors(this.fly.t0, this.fly.t1, k);
        if (f >= 1) this.fly = null;
      }
      this.controls.update();
      // Fog by how far the camera stands back, so the city reads at any zoom.
      const back = this.camera.position.distanceTo(this.controls.target);
      this.uni.uFogNear.value = back * 1.1;
      this.uni.uFogFar.value = back * 3.5;
      this.renderer.render(this.scene, this.camera);
      if (this.measuring) {
        this.measuring.gaps.push(dt * 1000);
        this.measuring.work.push(performance.now() - t0);
      }
    };
    this.raf = requestAnimationFrame(frame);
  }

  stop() {
    cancelAnimationFrame(this.raf);
    for (const f of this.off) f();
    this.controls.dispose();
    this.scene.traverse((o) => {
      const m = o as THREE.Mesh;
      m.geometry?.dispose();
    });
    this.renderer.dispose();
  }

  async measure(ms: number): Promise<{ fps: number; workP50: number; frames: number }> {
    this.measuring = { gaps: [], work: [] };
    await new Promise((r) => setTimeout(r, ms));
    const m = this.measuring;
    this.measuring = null;
    const gaps = m.gaps.slice(5);
    const work = [...m.work].sort((a, b) => a - b);
    const total = gaps.reduce((a, b) => a + b, 0);
    return { fps: gaps.length ? (1000 * gaps.length) / total : 0, workP50: work[Math.floor(work.length / 2)] ?? 0, frames: gaps.length };
  }
}

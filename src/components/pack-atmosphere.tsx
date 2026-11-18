import { Component, lazy, Suspense, useEffect, useRef, useState, useSyncExternalStore, type ReactNode } from 'react';
import StaggeredText from './react-bits/staggered-text';

const AuroraBlur = lazy(() => import('./react-bits/aurora-blur'));
const motionPreference = '(prefers-reduced-motion: reduce)';

function subscribeToMotion(notify: () => void) {
  const media = window.matchMedia(motionPreference);
  media.addEventListener('change', notify);
  return () => media.removeEventListener('change', notify);
}
function reducedMotion() {
  return window.matchMedia(motionPreference).matches;
}
function subscribeToVisibility(notify: () => void) {
  document.addEventListener('visibilitychange', notify);
  return () => document.removeEventListener('visibilitychange', notify);
}
function pageVisible() {
  return document.visibilityState === 'visible';
}

const auroraLayers = [
  { color: '#7543a8', speed: 0.24, intensity: 0.54 },
  { color: '#5953a5', speed: 0.14, intensity: 0.4 },
  { color: '#ac69bf', speed: 0.17, intensity: 0.23 },
  { color: '#53408b', speed: 0.08, intensity: 0.28 },
];
const auroraSky = [
  { color: '#1b1027', blend: 0.64 },
  { color: '#121025', blend: 0.42 },
];
const headlineStart = { opacity: 0.72, y: 8 };
const headlineEnd = { opacity: 1, y: 0 };

// The CSS sky remains visible if WebGL creation or the lazy chunk fails.
class AuroraBoundary extends Component<{ children: ReactNode }, { failed: boolean }> {
  state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  render() {
    return this.state.failed ? null : this.props.children;
  }
}

export function PackAtmosphere({ paused }: { paused: boolean }) {
  const sky = useRef<HTMLDivElement>(null);
  const [inView, setInView] = useState(false);
  const visible = useSyncExternalStore(subscribeToVisibility, pageVisible);
  const reduceMotion = useSyncExternalStore(subscribeToMotion, reducedMotion);
  const animated = inView && visible && !reduceMotion && !paused;

  useEffect(() => {
    const element = sky.current;
    if (!element || typeof IntersectionObserver === 'undefined') return;
    const observer = new IntersectionObserver(([entry]) => setInView(entry.isIntersecting));
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  return <div className="pack-atmosphere" ref={sky} data-animated={animated} aria-hidden="true">
    <AuroraBoundary>
      {animated && <Suspense fallback={null}>
        <AuroraBlur
          className="pack-aurora"
          layers={auroraLayers}
          skyLayers={auroraSky}
          speed={0.62}
          noiseScale={2.3}
          movementX={-0.8}
          movementY={-0.45}
          verticalFade={0.56}
          bloomIntensity={1.9}
          brightness={0.95}
          opacity={0.9}
        />
      </Suspense>}
    </AuroraBoundary>
    <div className="pack-sparks"><i/><i/><i/><i/><i/><i/></div>
  </div>;
}

export function PackHeadline({ paused }: { paused: boolean }) {
  const reduceMotion = useSyncExternalStore(subscribeToMotion, reducedMotion);
  return <h1 id="pack-title">
    {reduceMotion || paused ? <>Make something<br/>extraordinary.</> : <>
      <span className="sr-only">Make something extraordinary.</span>
      <span aria-hidden="true">
        <StaggeredText
          as="span"
          text={'Make something\nextraordinary.'}
          segmentBy="lines"
          from={headlineStart}
          to={headlineEnd}
          blur={false}
          duration={0.42}
          delay={100}
          easing={[0.18, 0.75, 0.24, 1]}
        />
      </span>
    </>}
  </h1>;
}

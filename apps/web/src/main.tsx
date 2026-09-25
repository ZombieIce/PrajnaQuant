import { lazy, Suspense } from 'react';
import { createRoot } from 'react-dom/client';
import { Header, PageErrorBoundary } from './shared';
import './styles.css';

const FactorDirectory = lazy(() => import('./pages/FactorDirectory'));
const FactorDetail = lazy(() => import('./pages/FactorDetail'));
const StrategyDirectory = lazy(() => import('./pages/StrategyDirectory'));
const StrategyDetail = lazy(() => import('./pages/StrategyDetail'));
const UniverseDirectory = lazy(() => import('./pages/UniverseDirectory'));
const MarketPage = lazy(() => import('./pages/MarketPage'));

function Page() {
  const path = location.pathname;
  const route = path === '/market' ? <MarketPage />
    : path === '/factors' ? <FactorDirectory />
    : path.startsWith('/factors/') ? <FactorDetail />
      : path === '/universes' ? <UniverseDirectory />
        : path.startsWith('/strategies/') ? <StrategyDetail />
          : <StrategyDirectory />;
  return <PageErrorBoundary><Suspense fallback={<><Header /><main><div className="empty-state">正在载入页面…</div></main></>}>{route}</Suspense></PageErrorBoundary>;
}

createRoot(document.getElementById('root')!).render(<Page />);

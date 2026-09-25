import { useEffect, useRef } from 'react';
import type { EChartsOption } from 'echarts';
import { use, init } from 'echarts/core';
import { BarChart, CandlestickChart, LineChart } from 'echarts/charts';
import { DataZoomComponent, GridComponent, LegendComponent, TooltipComponent } from 'echarts/components';
import { CanvasRenderer } from 'echarts/renderers';

use([BarChart, CandlestickChart, LineChart, DataZoomComponent, GridComponent, LegendComponent, TooltipComponent, CanvasRenderer]);

export function Chart({ option }: { option: EChartsOption }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!ref.current) return;
    const chart = init(ref.current);
    chart.setOption(option);
    const resize = () => chart.resize();
    addEventListener('resize', resize);
    return () => { removeEventListener('resize', resize); chart.dispose(); };
  }, [option]);
  return <div className="chart" ref={ref} />;
}

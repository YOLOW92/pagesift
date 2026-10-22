// Fixed, public AdaptivePlaywrightCrawler API; outputs are read from its dataset.
import { createRequire } from 'node:module';
import { readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
const require = createRequire(resolve('bench/data/six/node/package.json'));
const { AdaptivePlaywrightCrawler, log } = require('@crawlee/playwright');
log.setLevel(log.LEVELS.ERROR);
const [manifest, output] = process.argv.slice(2);
const requests = JSON.parse(readFileSync(manifest, 'utf8'));
const crawler = new AdaptivePlaywrightCrawler({
    maxConcurrency: 1, maxRequestsPerCrawl: requests.length, maxRequestRetries: 0,
    useSessionPool: false, respectRobotsTxtFile: true,
    launchContext: { launchOptions: { executablePath: '/opt/google/chrome/google-chrome', headless: true } },
    renderingTypeDetectionRatio: 0.1,
    async requestHandler({ request, parseWithCheerio, pushData }) {
        const $ = await parseWithCheerio('article');
        await pushData({ url: request.url, text: $('article').text() });
    },
});
await crawler.run(requests);
const dataset = await crawler.getDataset();
const data = await dataset.getData();
writeFileSync(output, JSON.stringify({ items: data.items, stats: crawler.stats.state }, null, 2) + '\n');

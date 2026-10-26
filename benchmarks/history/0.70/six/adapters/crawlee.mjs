// Conventional CheerioCrawler + enqueueLinks; no extra discovery or Markdown engine.
import { createRequire } from 'node:module';
import { writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
const require = createRequire(resolve('bench/data/six/node/package.json'));
const { CheerioCrawler, log } = require('@crawlee/cheerio');
log.setLevel(log.LEVELS.ERROR);
const [url, output] = process.argv.slice(2);
const pages = [];
const errors = [];
const crawler = new CheerioCrawler({
    maxConcurrency: 2, minConcurrency: 1, maxRequestsPerCrawl: 50,
    maxRequestRetries: 1, useSessionPool: false, respectRobotsTxtFile: true,
    requestHandlerTimeoutSecs: 10,
    async requestHandler({ request, $, enqueueLinks }) {
        pages.push({ url: request.loadedUrl || request.url, title: $('h1').text(),
                     body_chars: $('body').text().length });
        await enqueueLinks({ strategy: 'same-origin' });
    },
    async failedRequestHandler({ request }, error) {
        errors.push({ url: request.url, error: error.message });
    },
});
await crawler.run([url]);
writeFileSync(output, JSON.stringify({ pages, errors }, null, 2) + '\n');

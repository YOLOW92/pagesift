import { createRequire } from 'node:module';
import { resolve } from 'node:path';
import { writeFileSync } from 'node:fs';
const require = createRequire(resolve('bench/data/six/node/package.json'));
const { Firecrawl } = require('@mendable/firecrawl-js');
const [apiUrl, origin, output] = process.argv.slice(2);
const client = new Firecrawl({ apiKey: 'test-key', apiUrl });
const checks = [];
const check = (name, ok) => { checks.push({name, ok: Boolean(ok)}); };
try {
    const doc = await client.scrape(origin, { formats: ['markdown', 'links'] });
    check('scrape-markdown', doc.markdown?.includes('SDK_TEST_ARTICLE'));
    check('scrape-metadata', doc.metadata?.statusCode === 200);
    check('scrape-links', doc.links?.length >= 2);
    const mapped = await client.map(origin, { sitemap: 'skip', limit: 10 });
    check('map', mapped.links?.length >= 2);
    const batch = await client.batchScrape([origin, origin + 'article'], { formats: ['markdown'], pollInterval: 1, timeout: 30 });
    check('batch-polling', batch.status === 'completed' && batch.data?.length === 2);
    const crawl = await client.crawl(origin, { sitemap: 'skip', limit: 3, maxDiscoveryDepth: 1, pollInterval: 1, timeout: 30 });
    check('crawl-polling', crawl.status === 'completed' && crawl.data?.length >= 2);
    const job = await client.startCrawl(origin + 'slow', { limit: 1 });
    check('cancel', await client.cancelCrawl(job.id));
    try {
        await new Firecrawl({ apiKey: 'wrong', apiUrl }).scrape(origin);
        check('unauthorized', false);
    } catch(e) { check('unauthorized', /401|unauthoriz|invalid.*key/i.test(String(e))); }
} catch(e) { checks.push({ name:'unexpected-error', ok:false, error:String(e), stack:e.stack }); }
writeFileSync(output, JSON.stringify({sdk:'@mendable/firecrawl-js@4.28.1',checks}, null, 2)+'\n');
if (checks.some(c => !c.ok)) process.exitCode = 1;

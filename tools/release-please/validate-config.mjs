import {readFileSync} from 'node:fs';
import {dirname, join} from 'node:path';
import {createRequire} from 'node:module';

import Ajv from 'ajv';
import addFormats from 'ajv-formats';

const require = createRequire(import.meta.url);
const releasePleaseRoot = dirname(require.resolve('release-please/package.json'));
const ajv = new Ajv({allErrors: true, strict: false});
addFormats(ajv);

const inputs = [
  ['config.json', process.argv[2]],
  ['manifest.json', process.argv[3]],
];

if (inputs.some(([, path]) => !path)) {
  process.stderr.write('usage: validate-config.mjs <config> <manifest>\n');
  process.exit(2);
}

let valid = true;
for (const [schemaName, inputPath] of inputs) {
  const schema = JSON.parse(
    readFileSync(join(releasePleaseRoot, 'schemas', schemaName), 'utf8'),
  );
  const input = JSON.parse(readFileSync(inputPath, 'utf8'));
  const validate = ajv.compile(schema);
  if (!validate(input)) {
    process.stderr.write(
      `${inputPath}: ${ajv.errorsText(validate.errors)}\n`,
    );
    valid = false;
  }
}

if (!valid) {
  process.exit(1);
}

process.stdout.write('release-please configuration is valid\n');

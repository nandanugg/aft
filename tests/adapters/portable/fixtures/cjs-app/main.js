const { applyTax } = require('./lib/price');

function checkout(amount) {
  return applyTax(amount);
}

module.exports = { checkout };

//! tests/ts/fixtures.ts `Provider`: the synthetic Domino’s, website and Adyen the drop-in cases
//! ran against, with its counters and switches. Its assertions on what the binary sends become
//! recorded failures (the request then gets no answer, as a thrown fetch); `check` fails the test
//! on any.
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{Value, json};

use super::{Reply, Request, iso, now};

pub struct State {
    /// Every request that reached the provider (fixtures.ts `requests.length`).
    pub requests: usize,
    pub orders: usize,
    pub payments: usize,
    pub refreshes: usize,
    pub minor_amount: i64,
    pub lose_payment_response: bool,
    pub lose_order_response: bool,
    pub accept_checkout: bool,
    pub require_verification: bool,
    pub latest_paid: bool,
    failures: Vec<String>,
}

#[derive(Clone)]
pub struct Provider(Arc<Mutex<State>>);

/// The website page: the frozen fixture's synthetic menu, with a script that must never run.
pub fn html() -> String {
    let frozen = super::frozen_exchange("create-ready", "/website/panta/pizzur");
    frozen["response"]["body"].as_str().unwrap().to_owned()
}

pub fn profile() -> Value {
    json!({
        "id": 1,
        "name": "Test shopper",
        "phoneNumber": "3545550123",
        "email": null,
        "savedAddress": [{
            "AddressID": 42,
            "Address": "Test street 7",
            "PostalCode": "100",
            "PostalCodeName": "Test city",
            "AdditionalInfo": "DO-NOT-RETURN"
        }],
        "savedOrders": [],
        "access_token": "DO-NOT-RETURN"
    })
}

pub fn store() -> Value {
    json!({
        "RefID": "1",
        "Address": "Test street",
        "City": "Test",
        "Zip": "100",
        "AcceptInternet": true,
        "AcceptsPickup": true,
        "AcceptsDelivery": true,
        "Disabled": false,
        "IsHidden": false,
        "Status": 1,
        "StoreStatus": 1,
        "PickupQuote": "10-15",
        "DeliveryQuote": "20-30",
        "OpensAt": "11:00",
        "ClosesAt": "23:00",
        "OpeningHours": "11-23",
        "NotificationText": ""
    })
}

/// fixtures.ts `cart`.
pub fn cart() -> Value {
    json!({
        "fulfillment": {"type": "pickup", "storeId": "1"},
        "pizzas": [{"sizeId": "LG", "crustId": "HANDTOSS", "sections": [{"pizzaId": "TEST"}]}]
    })
}

impl Default for Provider {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(State {
            requests: 0,
            orders: 0,
            payments: 0,
            refreshes: 0,
            minor_amount: 250_000,
            lose_payment_response: false,
            lose_order_response: false,
            accept_checkout: true,
            require_verification: false,
            latest_paid: false,
            failures: Vec::new(),
        })))
    }
}

impl Provider {
    pub fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap()
    }

    /// Fails if the binary sent anything the provider asserted against.
    pub fn check(&self) {
        let failures = &self.state().failures;
        assert!(failures.is_empty(), "provider: {failures:?}");
    }

    fn fail(&self, message: String) -> Reply {
        self.state().failures.push(message);
        Reply::dropped()
    }

    pub fn answer(&self, request: &Request) -> Reply {
        self.state().requests += 1;
        let (path, query) = request
            .path
            .split_once('?')
            .unwrap_or((request.path.as_str(), ""));
        let authorization = request.headers.get("authorization").and_then(Value::as_str);

        if path.starts_with("/website/") {
            return Reply::text(200, &html());
        }
        if let Some(rest) = path.strip_prefix("/adyen/") {
            return self.payment(rest, query, authorization, &request.body);
        }
        if path == "/api/token" {
            self.state().refreshes += 1;
            return Reply::json(
                200,
                &json!({
                    "access_token": "rotated-access",
                    "refresh_token": "rotated-refresh",
                    "token_type": "bearer",
                    "username": "3545550123",
                    "expires_in": 3600
                }),
            );
        }
        if path == "/api/store" {
            return Reply::json(200, &json!([store()]));
        }
        if !matches!(
            authorization,
            Some("bearer synthetic-access" | "bearer rotated-access")
        ) {
            return self.fail(format!("{path}: authorization {authorization:?}"));
        }
        if path == "/api/user/newuser" {
            return Reply::json(200, &profile());
        }
        if path == "/api/orders" {
            if request.method != "POST" {
                return self.fail(format!("/api/orders with {}", request.method));
            }
            let body: Value = serde_json::from_str(&request.body).unwrap_or(Value::Null);

            if body["Pizzas"][0]["Sections"][0]["PizzaRefID"] != "TEST"
                || body["User"]["Username"] != "3545550123"
            {
                return self.fail(format!("/api/orders body {body}"));
            }
            if body["IsFinal"] != true {
                return Reply::json(200, &json!({"Total": 2500, "Success": true}));
            }
            if body["PayWithStraumur"] != true || body["Payonline"] != false {
                return self.fail(format!("final order body {body}"));
            }
            let mut state = self.state();
            state.orders += 1;

            if state.lose_order_response {
                return Reply::dropped();
            }
            return Reply::json(
                200,
                &json!({
                    "Total": 2500,
                    "Success": state.accept_checkout,
                    "OrderID": 123,
                    "OrderGuidId": "synthetic-order-guid",
                    "AdyenSessionId": "synthetic-session",
                    "AdyenSessionData": "private-initial-session",
                    "AdyenClientId": "private-client-key"
                }),
            );
        }
        if path == "/api/orders/cart/latest" {
            let latest = match self.state().latest_paid {
                true => json!({"OrderData": {"IsPayed": true}}),
                false => json!({"OrderData": ""}),
            };
            return Reply::json(200, &latest);
        }
        self.fail(format!("unexpected offline fixture request {path}"))
    }

    fn payment(&self, path: &str, query: &str, authorization: Option<&str>, body: &str) -> Reply {
        if authorization.is_some() || query != "clientKey=private-client-key" {
            return self.fail(format!("adyen {path}: {authorization:?} {query}"));
        }
        let body: Value = serde_json::from_str(body).unwrap_or(Value::Null);

        if path.ends_with("/setup") {
            if body != json!({"sessionData": "private-initial-session"}) {
                return self.fail(format!("setup body {body}"));
            }
            return Reply::json(
                200,
                &json!({
                    "id": "synthetic-session",
                    "sessionData": "private-rotated-session",
                    "expiresAt": iso(now() + 600_000),
                    "amount": {"currency": "ISK", "value": self.state().minor_amount},
                    "paymentMethods": {"storedPaymentMethods": [{
                        "id": "private-vault-token",
                        "type": "scheme",
                        "brand": "visa",
                        "lastFour": "1234",
                        "expiryMonth": "12",
                        "expiryYear": "2030"
                    }]}
                }),
            );
        }
        let expected = json!({
            "sessionData": "private-rotated-session",
            "paymentMethod": {"type": "scheme", "storedPaymentMethodId": "private-vault-token"},
            "storePaymentMethod": false
        });

        if !path.ends_with("/payments") || body != expected {
            return self.fail(format!("payment {path} body {body}"));
        }
        let mut state = self.state();
        state.payments += 1;

        if state.lose_payment_response {
            return Reply::dropped();
        }
        if state.require_verification {
            return Reply::json(
                200,
                &json!({
                    "resultCode": "ChallengeShopper",
                    "sessionData": "private-challenge-session",
                    "action": {"type": "threeDS2", "token": "private-challenge-token"}
                }),
            );
        }
        Reply::json(
            200,
            &json!({
                "resultCode": "Authorised",
                "sessionData": "private-final-session",
                "sessionResult": "private-result"
            }),
        )
    }
}

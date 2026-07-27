use anyhow::Result;
use embedded_mongodb::{Client, bson::doc};

fn main() -> Result<()> {
    // The database files are deleted when this temporary directory is dropped.
    let data_directory = tempfile::tempdir()?;
    let client = Client::new(data_directory.path())?;
    let database = client.database("shop");
    let orders = database.collection("orders");

    orders.insert_many([
        doc! {
            "customer": "Ada",
            "status": "paid",
            "items": [
                { "product": "Keyboard", "quantity": 1, "unit_price": 100 },
                { "product": "Mouse", "quantity": 2, "unit_price": 25 },
            ],
        },
        doc! {
            "customer": "Grace",
            "status": "pending",
            "items": [
                { "product": "Monitor", "quantity": 1, "unit_price": 250 },
            ],
        },
        doc! {
            "customer": "Linus",
            "status": "paid",
            "items": [
                { "product": "Keyboard", "quantity": 2, "unit_price": 90 },
                { "product": "Mouse", "quantity": 1, "unit_price": 25 },
            ],
        },
    ])?;

    // Paid orders -> individual items -> totals per product -> highest revenue first.
    let pipeline = [
        doc! { "$match": { "status": "paid" } },
        doc! { "$unwind": "$items" },
        doc! {
            "$group": {
                "_id": "$items.product",
                "units_sold": { "$sum": "$items.quantity" },
                "revenue": {
                    "$sum": {
                        "$multiply": ["$items.quantity", "$items.unit_price"],
                    },
                },
            },
        },
        doc! { "$sort": { "revenue": -1 } },
        doc! {
            "$project": {
                "_id": 0,
                "product": "$_id",
                "units_sold": 1,
                "revenue": 1,
            },
        },
    ];

    let sales_report = orders.aggregate(pipeline)?.try_collect()?;
    assert_eq!(sales_report.len(), 2);
    assert_eq!(sales_report[0].get_str("product")?, "Keyboard");
    println!("sales report: {sales_report:#?}");

    client.close()?;
    Ok(())
}

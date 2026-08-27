public class MyDvMessageData
{
	private int m_a;

	private string b;

	public int OffsetProgrammableMemoryAddress
	{
		set
		{
			this.m_a = value;
		}
	}

	public string MyDvMessage
	{
		get { return b; }
	}

	public void b(n7 A_0, int A_1)
	{
		int a_ = 331617 + this.m_a + 20 * A_1;
		A_0.d(MyDvMessage, a_, 20);
	}

	public void a(n7 A_0, int A_1)
	{
		int a_ = 331617 + this.m_a + 20 * A_1;
		b = A_0.g(a_, 20);
	}
}
